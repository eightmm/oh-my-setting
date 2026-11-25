//! Canonical producer acquisition and panel-state serialization.
use crate::dispatch::{WriteSlots, write_slots_for_main};
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
#[derive(Default)]
struct CachedState {
    attempts: Option<(String, Value)>,
    dispatches: Option<DispatchRead>,
    write_slots: Option<CachedWriteSlots>,
}

type Dispatches = serde_json::Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DispatchGeneration {
    key: String,
    size: Option<u64>,
    leaf_uid: Option<u32>,
}

struct DispatchRead {
    calls: Option<Dispatches>,
    generation: Option<DispatchGeneration>,
    capacity_valid: bool,
}

struct CachedWriteSlots {
    ledger: String,
    reservations: String,
    main: String,
    slots: WriteSlots,
}

// One projection per session bounds memory and prevents cross-repository reuse.
#[derive(Default)]
pub struct AttemptCache(Mutex<CachedState>);

fn path_generation(
    repo: &Path,
    segments: &[(&str, bool)],
    allow_missing: bool,
) -> Option<(String, Option<u64>, Option<u32>)> {
    use std::os::unix::fs::MetadataExt;
    let root = std::fs::canonicalize(repo).ok()?;
    let mut path = root.clone();
    let root_meta = std::fs::metadata(&root).ok()?;
    let mut size = root_meta.size();
    let mut leaf_uid = None;
    let mut parts = vec![format!("{}:{}", root_meta.dev(), root_meta.ino())];
    for &(name, directory) in segments {
        path.push(name);
        let m = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                parts.push(format!("missing:{name}"));
                return Some((format!("{:?}|{}", root, parts.join("|")), None, None));
            }
            Err(_) => return None,
        };
        if m.file_type().is_symlink()
            || (directory && !m.is_dir())
            || (!directory && (!m.is_file() || m.nlink() != 1))
        {
            return None;
        }
        size = m.size();
        let uid = m.uid();
        leaf_uid = Some(uid);
        parts.push(format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            uid,
            size,
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec()
        ));
    }
    Some((
        format!("{:?}|{}", root, parts.join("|")),
        Some(size),
        leaf_uid,
    ))
}

fn ledger_generation(repo: &Path) -> Option<String> {
    path_generation(
        repo,
        &[(".oms", true), ("lifecycle", true), ("events.jsonl", false)],
        false,
    )
    .map(|(generation, _, _)| generation)
}

fn dispatch_generation(repo: &Path) -> Option<DispatchGeneration> {
    path_generation(
        repo,
        &[
            (".oms", true),
            ("artifacts", true),
            ("panel-dispatch", true),
            ("reservations.json", false),
        ],
        true,
    )
    .map(|(key, size, leaf_uid)| DispatchGeneration {
        key,
        size,
        leaf_uid,
    })
}

fn read_dispatches(store: &Store, missing: bool) -> (Option<Dispatches>, bool) {
    let Ok(bytes) = store.read(
        Path::new(".oms/artifacts/panel-dispatch/reservations.json"),
        crate::storage::MAX_STATE,
        true,
    ) else {
        return (None, false);
    };
    // Optional reads also return empty bytes for a present empty file. Only an
    // observed missing path whose generation remains stable proves no calls.
    if bytes.is_empty() {
        return if missing {
            (Some(Dispatches::new()), true)
        } else {
            (None, false)
        };
    }
    if missing {
        return (None, false);
    }
    let Ok(raw) = crate::decode(&bytes, crate::storage::MAX_STATE) else {
        return (None, false);
    };
    if raw["schema"] != 1 {
        return (None, false);
    }
    let Some(calls) = raw["calls"].as_object() else {
        return (None, false);
    };
    let mut out = serde_json::Map::new();
    for (member, entry) in calls {
        if !entry.is_object() {
            continue;
        }
        let route = &entry["route"];
        out.insert(
            member.clone(),
            serde_json::json!({
                "member": member,
                "room": text(entry, "room"),
                "task_id": text(entry, "task_id"),
                "main": text(entry, "main"),
                "main_attempt": text(entry, "main_attempt"),
                "state": text(entry, "state"),
                "exit": entry.get("exit").and_then(Value::as_i64),
                "created_at": text(entry, "created_at"),
                "route": {
                    "access": text(route, "access"),
                    "kind": text(route, "kind"),
                    "role": text(route, "role"),
                    "provider": text(route, "provider"),
                    "model": text(route, "model"),
                    "purpose": text(route, "purpose"),
                }
            }),
        );
    }
    let capacity_valid =
        oms_command_telemetry::store::dispatch::validate_registry_bytes(&bytes).is_ok();
    (Some(out), capacity_valid)
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
        let (generation, attempts) = cache.attempts.as_ref()?;
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
        self.attempts_with_generation(repo, service, stop)
            .map(|(attempts, _)| attempts)
    }

    fn attempts_with_generation(
        &self,
        repo: &Path,
        service: &dyn Services,
        stop: &AtomicBool,
    ) -> Result<(Value, Option<String>)> {
        let mut cache = self
            .0
            .lock()
            .map_err(|_| Error::new("attempt cache unavailable"))?;
        let before = ledger_generation(repo);
        if !stop.load(Ordering::Relaxed)
            && let Some(ref generation) = before
            && let Some((key, value)) = cache.attempts.as_ref()
            && key == generation
            && ledger_generation(repo).as_ref() == Some(generation)
        {
            return Ok((value.clone(), Some(generation.clone())));
        }
        cache.attempts = None;
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
            cache.attempts = Some((generation.clone(), attempts.clone()));
            return Ok((attempts, Some(generation)));
        }
        Ok((attempts, None))
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
            && let Some((key, attempts)) = cache.attempts.as_ref()
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
            cache.attempts = None;
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
                cache.attempts = None;
                continue;
            }
            if let Some(generation) = after {
                cache.attempts = Some((generation, attempts));
            }
            return Ok(selected);
        }
        Err(Error::new("lifecycle ledger changed during verification"))
    }

    fn dispatches(&self, repo: &Path, store: &Store, stop: &AtomicBool) -> DispatchRead {
        let unavailable = DispatchRead {
            calls: None,
            generation: None,
            capacity_valid: false,
        };
        let Ok(mut cache) = self.0.lock() else {
            return unavailable;
        };
        let Some(before) = dispatch_generation(repo) else {
            cache.dispatches = None;
            return unavailable;
        };
        if !stop.load(Ordering::Relaxed)
            && let Some(cached) = cache.dispatches.as_ref()
            && cached.generation.as_ref() == Some(&before)
            && dispatch_generation(repo).as_ref() == Some(&before)
        {
            return DispatchRead {
                calls: cached.calls.clone(),
                generation: Some(before),
                capacity_valid: cached.capacity_valid,
            };
        }
        cache.dispatches = None;
        let (calls, capacity_valid) = read_dispatches(store, before.size.is_none());
        let generation = (!stop.load(Ordering::Relaxed)
            && dispatch_generation(repo).as_ref() == Some(&before))
        .then_some(before);
        if let Some(generation) = &generation {
            cache.dispatches = Some(DispatchRead {
                calls: calls.clone(),
                generation: Some(generation.clone()),
                capacity_valid,
            });
        }
        DispatchRead {
            calls,
            generation,
            capacity_valid,
        }
    }

    fn write_slots(
        &self,
        main: &str,
        attempts: &Value,
        ledger: &str,
        dispatches: &DispatchRead,
    ) -> Option<WriteSlots> {
        self.write_slots_for_uid(
            main,
            attempts,
            ledger,
            dispatches,
            rustix::process::getuid().as_raw(),
        )
    }

    fn write_slots_for_uid(
        &self,
        main: &str,
        attempts: &Value,
        ledger: &str,
        dispatches: &DispatchRead,
        current_uid: u32,
    ) -> Option<WriteSlots> {
        if !dispatches.capacity_valid {
            return None;
        }
        let calls = dispatches.calls.as_ref()?;
        let generation = dispatches.generation.as_ref()?;
        if generation
            .size
            .is_some_and(|size| size > oms_command_telemetry::store::dispatch::MAX_REGISTRY as u64)
        {
            return None;
        }
        if generation.leaf_uid.is_some_and(|uid| uid != current_uid) {
            return None;
        }
        // These values and generations are local to the collecting snapshot.
        // Other callers can replace the cache's input entries before we lock it.
        let mut cache = self.0.lock().ok()?;
        if let Some(warm) = &cache.write_slots
            && warm.ledger == ledger
            && warm.reservations == generation.key
            && warm.main == main
        {
            return Some(warm.slots);
        }
        let slots = write_slots_for_main(array(attempts), calls, main);
        cache.write_slots = Some(CachedWriteSlots {
            ledger: ledger.to_owned(),
            reservations: generation.key.clone(),
            main: main.to_owned(),
            slots,
        });
        Some(slots)
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
    snapshot_scoped(repo, room_id, service, stop, cache, None).map(|(snapshot, _)| snapshot)
}

/// Collect display-only WIP alongside the same snapshot inputs. The Board
/// still verifies its exact live Room and Main scope before publishing either.
pub fn snapshot_with_write_slots(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
    cache: &AttemptCache,
    main: Option<&str>,
) -> Result<(Snapshot, Option<WriteSlots>)> {
    snapshot_scoped(repo, room_id, service, stop, Some(cache), main)
}

fn snapshot_scoped(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
    cache: Option<&AttemptCache>,
    main: Option<&str>,
) -> Result<(Snapshot, Option<WriteSlots>)> {
    let result = snapshot_inner(repo, room_id, service, stop, cache, main);
    if result.is_err()
        && let Some(cache) = cache
        && let Ok(mut entry) = cache.0.lock()
    {
        entry.attempts = None;
        entry.dispatches = None;
        entry.write_slots = None;
    }
    result
}

fn attempt_matches_room(rid: &str, main_attempt: &str, attempt: &Value) -> bool {
    let refs = &attempt["refs"];
    let rooms = [text(refs, "panel_room_id"), text(refs, "panel_room")];
    if rooms.iter().any(|r| !r.is_empty() && r != rid) {
        return false;
    }
    rooms.iter().any(|r| r == rid)
        || (!main_attempt.is_empty() && text(attempt, "attempt_id") == main_attempt)
}

fn snapshot_inner(
    repo: &Path,
    room_id: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
    cache: Option<&AttemptCache>,
    main: Option<&str>,
) -> Result<(Snapshot, Option<WriteSlots>)> {
    let (attempts, ledger) = if let Some(cache) = cache {
        cache.attempts_with_generation(repo, service, stop)?
    } else {
        (
            query(
                service,
                NativeCommand::Events,
                repo,
                &["list", "--json"],
                stop,
            )?,
            None,
        )
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
    let mut room_dispatches = serde_json::Map::new();
    let mut write_slots = None;
    if let Some(rid) = room_id {
        let main_attempt = text(&room, "main");
        for attempt in array(&attempts) {
            if !attempt_matches_room(rid, &main_attempt, attempt) {
                continue;
            }
            let attempt_id = text(attempt, "attempt_id");
            if !attempt_id.is_empty() {
                room_attempts.insert(attempt_id, attempt.clone());
            }
            let participant = text(&attempt["refs"], "panel_room_participant");
            if !participant.is_empty() {
                let replace = room_attempts.get(&participant).is_none_or(|prev| {
                    // Participant aliases must never replace an exact attempt ID.
                    text(prev, "attempt_id") != participant
                        && (LIVE_STATES.contains(&text(attempt, "state").as_str())
                            || !LIVE_STATES.contains(&text(prev, "state").as_str()))
                });
                if replace {
                    room_attempts.insert(participant, attempt.clone());
                }
            }
        }
        let enrolled: std::collections::BTreeSet<String> = array(&room["participants"])
            .iter()
            .map(|p| text(p, "participant"))
            .filter(|id| !id.is_empty())
            .collect();
        let dispatches = if let Some(cache) = cache {
            cache.dispatches(repo, &store, stop)
        } else {
            let (calls, capacity_valid) = read_dispatches(&store, false);
            DispatchRead {
                calls,
                generation: None,
                capacity_valid,
            }
        };
        if let (Some(main), Some(cache), Some(ledger)) = (main, cache, ledger.as_deref())
            && room["id"] == rid
            && room["closed"] == false
            && room["successor"].is_null()
            && let Some(attempt) = room_attempts.get(main)
            && attempt["attempt_id"] == main
            && attempt["tool"] == "panel-main"
            && attempt["terminal"] == false
            && attempt["refs"]["panel_room_id"] == rid
        {
            write_slots = cache.write_slots(main, &attempts, ledger, &dispatches);
        }
        for (member, entry) in dispatches.calls.into_iter().flatten() {
            if entry["room"] == rid && enrolled.contains(member.as_str()) {
                room_dispatches.insert(member, entry);
            }
        }
    }
    let now_sys = std::time::SystemTime::now();
    let now_epoch = now_sys
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let generated_at = chrono::DateTime::<chrono::Utc>::from_timestamp(now_epoch, 0)
        .map(|v| v.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let (managed_scratch, scratch_peers) =
        crate::scratch_activity::observe_with_peer_runs(repo, stop);
    let attempt_lists = serde_json::json!({"active": active, "recent": recent});
    let board = crate::board::project_with_scratch(
        repo,
        &room,
        &attempt_lists,
        &room_attempts,
        &store,
        &scratch_peers,
        now_epoch,
        now_sys,
    );
    let report = serde_json::json!({
        "schema": 1,
        "generated_at": generated_at,
        "room": room,
        "plan": plan,
        "attempts": attempt_lists,
        "room_attempts": room_attempts,
        "room_dispatches": room_dispatches,
        "managedScratch": managed_scratch,
        "board": board,
        "collection": {"ok": true},
        "capacity_stale": false
    });
    Ok((Snapshot { report, status: 0 }, write_slots))
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

    struct SnapshotTestService {
        events_calls: std::sync::atomic::AtomicUsize,
        room_calls: std::sync::atomic::AtomicUsize,
        attempts: Mutex<Value>,
        room: Mutex<Value>,
        room_error: Mutex<Option<String>>,
    }

    impl SnapshotTestService {
        fn new(attempts: Value, room: Value) -> Self {
            Self {
                events_calls: std::sync::atomic::AtomicUsize::new(0),
                room_calls: std::sync::atomic::AtomicUsize::new(0),
                attempts: Mutex::new(attempts),
                room: Mutex::new(room),
                room_error: Mutex::new(None),
            }
        }
    }

    impl crate::services::PeerExecutor for SnapshotTestService {
        fn execute(
            &self,
            _request: &crate::services::ExecutionRequest,
            _interrupted: &AtomicBool,
        ) -> Result<crate::services::CommandOutput> {
            Err(Error::new("unexpected external process"))
        }
    }

    impl Services for SnapshotTestService {
        fn command(
            &self,
            command: NativeCommand,
            _repo: &Path,
            args: &[std::ffi::OsString],
            _interrupted: &AtomicBool,
        ) -> Result<crate::services::CommandOutput> {
            match command {
                NativeCommand::Events => {
                    assert_eq!(args, ["list", "--json"]);
                    self.events_calls.fetch_add(1, Ordering::Relaxed);
                    let stdout = serde_json::to_vec(&*self.attempts.lock().unwrap()).unwrap();
                    Ok(crate::services::CommandOutput {
                        code: 0,
                        stdout,
                        stderr: Vec::new(),
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    })
                }
                NativeCommand::Room => {
                    self.room_calls.fetch_add(1, Ordering::Relaxed);
                    if let Some(msg) = self.room_error.lock().unwrap().as_ref() {
                        return Ok(crate::services::CommandOutput {
                            code: 2,
                            stdout: Vec::new(),
                            stderr: msg.as_bytes().to_vec(),
                            cleanup_complete: true,
                            output_complete: true,
                            limited: false,
                        });
                    }
                    let stdout = serde_json::to_vec(&*self.room.lock().unwrap()).unwrap();
                    Ok(crate::services::CommandOutput {
                        code: 0,
                        stdout,
                        stderr: Vec::new(),
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    })
                }
                other => panic!("unexpected command: {other:?}"),
            }
        }
    }

    #[test]
    fn room_attempts_isolates_rooms_contradictory_refs_and_prefers_live_participant_attempts() {
        let base = TempDir::new("room-attempts-isolation");
        let repo = base.0.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let attempts = serde_json::json!([
            {
                "attempt_id": "att_main_clean",
                "state": "working",
                "refs": {}
            },
            {
                "attempt_id": "att_main_foreign",
                "state": "working",
                "refs": {"panel_room_id": "room-other"}
            },
            {
                "attempt_id": "att_room_id_only",
                "state": "working",
                "refs": {"panel_room_id": "room-a"}
            },
            {
                "attempt_id": "att_room_alt_only",
                "state": "done",
                "refs": {"panel_room": "room-a"}
            },
            {
                "attempt_id": "att_foreign_id",
                "state": "working",
                "refs": {"panel_room_id": "room-b"}
            },
            {
                "attempt_id": "att_foreign_alt",
                "state": "working",
                "refs": {"panel_room": "room-b"}
            },
            {
                "attempt_id": "att_contradict_1",
                "state": "working",
                "refs": {"panel_room_id": "room-a", "panel_room": "room-b"}
            },
            {
                "attempt_id": "att_contradict_2",
                "state": "working",
                "refs": {"panel_room_id": "room-b", "panel_room": "room-a"}
            },
            {
                "attempt_id": "att_p1_live",
                "state": "working",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P1"}
            },
            {
                "attempt_id": "att_p1_done",
                "state": "done",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P1"}
            },
            {
                "attempt_id": "att_p2_done",
                "state": "failed",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P2"}
            },
            {
                "attempt_id": "att_p2_live",
                "state": "running",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P2"}
            },
            {
                "attempt_id": "att_p3_1",
                "state": "starting",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P3"}
            },
            {
                "attempt_id": "att_p3_2",
                "state": "working",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P3"}
            },
            {
                "attempt_id": "att_p4_1",
                "state": "failed",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P4"}
            },
            {
                "attempt_id": "att_p4_2",
                "state": "done",
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "P4"}
            }
        ]);
        let room = serde_json::json!({
            "id": "room-a",
            "main": "att_main_clean",
            "participants": []
        });
        let service = SnapshotTestService::new(attempts, room);
        let stop = AtomicBool::new(false);

        let snap = snapshot(&repo, Some("room-a"), &service, &stop).unwrap();
        let ra = snap.report["room_attempts"].as_object().unwrap();

        assert!(ra.contains_key("att_main_clean"));
        assert!(ra.contains_key("att_room_id_only"));
        assert!(ra.contains_key("att_room_alt_only"));
        assert!(!ra.contains_key("att_main_foreign"));
        assert!(!ra.contains_key("att_foreign_id"));
        assert!(!ra.contains_key("att_foreign_alt"));
        assert!(!ra.contains_key("att_contradict_1"));
        assert!(!ra.contains_key("att_contradict_2"));

        // Distinct attempt_id entries are all retained for room-a.
        for id in [
            "att_p1_live",
            "att_p1_done",
            "att_p2_done",
            "att_p2_live",
            "att_p3_1",
            "att_p3_2",
            "att_p4_1",
            "att_p4_2",
        ] {
            assert!(ra.contains_key(id), "missing attempt_id {id}");
        }

        // Participant key preserves live over terminal, and otherwise prefers latest matching state class.
        assert_eq!(ra["P1"]["attempt_id"], "att_p1_live");
        assert_eq!(ra["P2"]["attempt_id"], "att_p2_live");
        assert_eq!(ra["P3"]["attempt_id"], "att_p3_2");
        assert_eq!(ra["P4"]["attempt_id"], "att_p4_2");

        // Even if room.main names att_main_foreign, its foreign panel_room_id ref disqualifies it.
        *service.room.lock().unwrap() = serde_json::json!({
            "id": "room-a",
            "main": "att_main_foreign",
            "participants": []
        });
        let snap2 = snapshot(&repo, Some("room-a"), &service, &stop).unwrap();
        let ra2 = snap2.report["room_attempts"].as_object().unwrap();
        assert!(!ra2.contains_key("att_main_foreign"));
        assert!(!ra2.contains_key("att_main_clean"));
    }

    #[test]
    fn room_attempts_exact_ids_win_over_live_participant_aliases_in_any_order() {
        let base = TempDir::new("room-attempt-id-precedence");
        let repo = base.0.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let main = serde_json::json!({
            "attempt_id": "main-attempt", "tool": "panel-main", "state": "working",
            "refs": {"panel_room_id": "room-a", "panel_room_participant": "main-attempt"}
        });
        let council = serde_json::json!({
            "attempt_id": "council-attempt", "tool": "peer-council", "state": "queued",
            "refs": {"panel_room_id": "room-a", "panel_room_participant": "main-attempt"}
        });
        let returned = serde_json::json!({
            "attempt_id": "returned-attempt", "state": "done",
            "refs": {"panel_room_id": "room-a", "panel_room_participant": "council-attempt"}
        });
        let orphan = serde_json::json!({
            "attempt_id": "orphan-attempt", "state": "working",
            "refs": {"panel_room_id": "room-a"}
        });
        let foreign = serde_json::json!({
            "attempt_id": "foreign-attempt", "state": "running",
            "refs": {"panel_room_id": "room-other", "panel_room_participant": "main-attempt"}
        });
        let room = serde_json::json!({
            "id": "room-a", "main": "main-attempt",
            "participants": [{"participant": "main-attempt", "role": "main"}]
        });
        let service = SnapshotTestService::new(serde_json::json!([]), room);
        let stop = AtomicBool::new(false);
        let rows = [&main, &council, &returned];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            *service.attempts.lock().unwrap() = serde_json::json!([
                rows[order[0]],
                rows[order[1]],
                rows[order[2]],
                orphan,
                foreign
            ]);
            let report = snapshot(&repo, Some("room-a"), &service, &stop)
                .unwrap()
                .report;
            let attempts = &report["room_attempts"];
            for expected in [&main, &council, &returned, &orphan] {
                assert_eq!(
                    &attempts[text(expected, "attempt_id")],
                    expected,
                    "iteration order: {order:?}"
                );
            }
            assert_eq!(attempts.as_object().unwrap().len(), 4);
            assert!(attempts.get("foreign-attempt").is_none());
        }
    }

    #[test]
    fn snapshot_write_slots_keep_the_events_used_for_the_frame() {
        let base = TempDir::new("write-slots-overlapping-cache");
        let repo = base.0.join("repo");
        let ledger = repo.join(".oms/lifecycle/events.jsonl");
        let reservations = repo.join(".oms/artifacts/panel-dispatch/reservations.json");
        for path in [&ledger, &reservations] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&ledger, b"earlier ledger").unwrap();
        std::fs::write(&reservations, br#"{"schema":1,"calls":{}}"#).unwrap();
        let main = serde_json::json!({
            "attempt_id":"main", "tool":"panel-main", "state":"working",
            "terminal":false, "refs":{"panel_room_id":"room"}
        });
        let review = serde_json::json!({
            "attempt_id":"review", "parent_attempt_id":"main", "state":"review",
            "refs":{"panel_room_id":"room","panel_access":"write"}
        });
        let service = SnapshotTestService::new(
            serde_json::json!([main, review]),
            serde_json::json!({"id":"room", "main":"main", "closed":false}),
        );
        let cache = AttemptCache::default();
        let stop = AtomicBool::new(false);

        struct ReplaceOnRoom<'a> {
            source: &'a SnapshotTestService,
            cache: &'a AttemptCache,
            ledger: &'a Path,
        }
        impl crate::services::PeerExecutor for ReplaceOnRoom<'_> {
            fn execute(
                &self,
                request: &crate::services::ExecutionRequest,
                stop: &AtomicBool,
            ) -> Result<crate::services::CommandOutput> {
                self.source.execute(request, stop)
            }
        }
        impl Services for ReplaceOnRoom<'_> {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[std::ffi::OsString],
                stop: &AtomicBool,
            ) -> Result<crate::services::CommandOutput> {
                if command == NativeCommand::Room {
                    self.source.attempts.lock().unwrap()[1]["state"] = "done".into();
                    std::fs::write(self.ledger, b"later ledger, new generation").unwrap();
                    let later = self.cache.attempts(repo, self.source, stop)?;
                    assert_eq!(later[1]["state"], "done");
                }
                self.source.command(command, repo, args, stop)
            }
        }

        let interleaved = ReplaceOnRoom {
            source: &service,
            cache: &cache,
            ledger: &ledger,
        };
        let (earlier, slots) = snapshot_with_write_slots(
            &repo,
            Some("room"),
            &interleaved,
            &stop,
            &cache,
            Some("main"),
        )
        .unwrap();
        assert_eq!(earlier.report["room_attempts"]["review"]["state"], "review");
        assert_eq!(
            slots,
            Some(WriteSlots {
                awaiting: 1,
                ..WriteSlots::default()
            })
        );
        let (later, slots) =
            snapshot_with_write_slots(&repo, Some("room"), &service, &stop, &cache, Some("main"))
                .unwrap();
        assert_eq!(later.report["room_attempts"]["review"]["state"], "done");
        assert_eq!(slots, Some(WriteSlots::default()), "proved empty is zero");
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn room_dispatches_projects_scoped_compact_entries_caches_and_rejects_symlinks() {
        use std::os::unix::fs::{MetadataExt, symlink};

        let base = TempDir::new("room-dispatches-scope");
        let repo = base.0.join("repo");
        let lifecycle = repo.join(".oms/lifecycle");
        let dispatch_dir = repo.join(".oms/artifacts/panel-dispatch");
        std::fs::create_dir_all(&lifecycle).unwrap();
        std::fs::create_dir_all(dispatch_dir.parent().unwrap()).unwrap();
        std::fs::write(lifecycle.join("events.jsonl"), b"").unwrap();
        let reservations = dispatch_dir.join("reservations.json");

        let mut attempts = serde_json::json!([
            {
                "attempt_id": "att_main",
                "tool": "panel-main",
                "state": "working",
                "terminal": false,
                "refs": {"panel_room_id": "room-a", "panel_room_participant": "main-codex"}
            }
        ]);
        for number in 0..8 {
            attempts.as_array_mut().unwrap().push(serde_json::json!({
                "attempt_id": format!("attempt-review-{number}"),
                "parent_attempt_id": "att_main",
                "state": "review",
                "refs": {"panel_room_id": "room-a", "panel_access": "write",
                         "panel_room_participant": format!("call-review-{number}")}
            }));
        }
        attempts.as_array_mut().unwrap().extend([
            serde_json::json!({
                "attempt_id":"attempt-read", "parent_attempt_id":"att_main", "state":"review",
                "refs":{"panel_room_id":"room-a", "panel_access":"read"}
            }),
            serde_json::json!({
                "attempt_id":"attempt-foreign", "parent_attempt_id":"att_other", "state":"review",
                "refs":{"panel_room_id":"room-a", "panel_access":"write"}
            }),
        ]);
        let room = serde_json::json!({
            "id": "room-a",
            "closed": false,
            "main": "att_main",
            "participants": [
                {"participant": "main-codex", "role": "main", "joined": true},
                {"participant": "call-enrolled", "role": "builder", "joined": true},
                {"participant": "call-reserved", "role": "reviewer", "joined": false},
                {"participant": "call-foreign-member", "role": "worker", "joined": true}
            ]
        });
        let service = SnapshotTestService::new(attempts, room);
        let cache = AttemptCache::default();
        let stop = AtomicBool::new(false);

        let collect = |main| {
            snapshot_with_write_slots(&repo, Some("room-a"), &service, &stop, &cache, main).unwrap()
        };
        // Both the missing directory and the missing leaf are a verified
        // empty registry. Existing eligible write attempts still count.
        let known_missing = Some(WriteSlots {
            awaiting: 8,
            ..WriteSlots::default()
        });
        let (initial, initial_slots) = collect(Some("att_main"));
        assert_eq!(initial.report["room_dispatches"], serde_json::json!({}));
        assert_eq!(initial_slots, known_missing);
        std::fs::create_dir(&dispatch_dir).unwrap();
        assert_eq!(collect(Some("att_main")).1, known_missing);
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);

        let full_registry = serde_json::json!({
            "schema": 1,
            "calls": {
                "call-enrolled": {
                    "schema": 2,
                    "member": "call-enrolled",
                    "room": "room-a",
                    "task_id": "T1",
                    "main": "main-codex",
                    "main_attempt": "att_main",
                    "state": "awaiting",
                    "exit": 0,
                    "created_at": "2026-11-22T00:00:00Z",
                    "physical": {"pid": 4242, "raw": "bulky"},
                    "scopes": ["crates/oms-panel-core/src/state.rs"],
                    "execution_repo": "/tmp/exec-repo",
                    "route": {
                        "access": "write",
                        "kind": "delegate",
                        "role": "builder",
                        "provider": "codex",
                        "model": "gpt-5.4",
                        "purpose": "implement",
                        "owner": "codex",
                        "workload": "medium",
                        "snapshot": {"schema": 1, "catalog": {"probe": [1, 2, 3]}}
                    }
                },
                "call-reserved": {
                    "schema": 2,
                    "member": "call-reserved",
                    "room": "room-a",
                    "task_id": "T2",
                    "main": "main-codex",
                    "main_attempt": "att_main",
                    "state": "reserved",
                    "created_at": "2026-11-22T00:00:01Z",
                    "route": {
                        "access": "read",
                        "kind": "review",
                        "role": "reviewer",
                        "provider": "claude",
                        "model": "sonnet",
                        "purpose": "verify",
                        "snapshot": {"schema": 1}
                    }
                },
                "call-unjoined-orphan": {
                    "schema": 2,
                    "member": "call-unjoined-orphan",
                    "room": "room-a",
                    "task_id": "T3",
                    "main": "main-codex",
                    "main_attempt": "att_main",
                    "state": "reserved",
                    "created_at": "2026-11-22T00:00:02Z",
                    "route": {
                        "access": "write",
                        "kind": "delegate",
                        "role": "worker",
                        "provider": "codex",
                        "model": "gpt-5.4",
                        "purpose": "orphan"
                    }
                },
                "call-unrecorded-running": {
                    "room": "room-a", "main_attempt": "att_main", "state": "running",
                    "route": {"access": "write"}
                },
                "call-review-0": {
                    "room": "room-a", "main_attempt": "att_main", "state": "running",
                    "route": {"access": "write"}
                },
                "call-foreign-member": {
                    "schema": 2,
                    "member": "call-foreign-member",
                    "room": "room-b",
                    "task_id": "T4",
                    "main": "main-claude",
                    "main_attempt": "att_other",
                    "state": "reserved",
                    "created_at": "2026-11-22T00:00:03Z",
                    "route": {
                        "access": "write",
                        "kind": "delegate",
                        "role": "worker",
                        "provider": "claude",
                        "model": "sonnet",
                        "purpose": "foreign"
                    }
                }
            }
        });
        let raw_bytes = serde_json::to_string(&full_registry).unwrap();
        std::fs::write(&reservations, &raw_bytes).unwrap();

        let (populated, slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        let rd = populated.report["room_dispatches"].as_object().unwrap();
        assert_eq!(rd.len(), 2);
        assert!(!rd.contains_key("call-unjoined-orphan"));
        assert!(!rd.contains_key("call-foreign-member"));
        assert!(populated.report.get("board_write_slots").is_none());
        let expected_slots = WriteSlots {
            active: 0,
            awaiting: 8,
            unconfirmed: 2,
        };
        assert_eq!(slots, Some(expected_slots));
        assert_eq!(collect(Some("att_other")).1, None);
        assert_eq!(collect(None).1, None);
        assert_eq!(collect(Some("att_main")).1, Some(expected_slots));
        assert_eq!(
            rd["call-enrolled"],
            serde_json::json!({
                "member": "call-enrolled",
                "room": "room-a",
                "task_id": "T1",
                "main": "main-codex",
                "main_attempt": "att_main",
                "state": "awaiting",
                "exit": 0,
                "created_at": "2026-11-22T00:00:00Z",
                "route": {
                    "access": "write",
                    "kind": "delegate",
                    "role": "builder",
                    "provider": "codex",
                    "model": "gpt-5.4",
                    "purpose": "implement"
                }
            })
        );
        assert_eq!(rd["call-reserved"]["state"], "reserved");
        assert_eq!(rd["call-reserved"].get("exit"), Some(&Value::Null));
        assert!(rd["call-reserved"]["route"].get("snapshot").is_none());

        // Unchanged refresh keeps 0 extra Events calls and returns identical room_dispatches.
        let (again, cached_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            again.report["room_dispatches"],
            populated.report["room_dispatches"]
        );
        assert_eq!(cached_slots, Some(expected_slots));

        // Updating reservations.json refreshes room_dispatches without invalidating attempts cache.
        let mut updated_registry = full_registry.clone();
        updated_registry["calls"]["call-reserved"]["state"] = "awaiting".into();
        updated_registry["calls"]["call-reserved"]["exit"] = 0.into();
        updated_registry["calls"]["call-unjoined-orphan"]["state"] = "settled".into();
        let updated_bytes = serde_json::to_string(&updated_registry).unwrap();
        std::fs::write(&reservations, &updated_bytes).unwrap();
        let (refreshed, refreshed_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            refreshed.report["room_dispatches"]["call-reserved"]["state"],
            "awaiting"
        );
        assert_eq!(
            refreshed.report["room_dispatches"]["call-reserved"]["exit"],
            0
        );

        assert_eq!(
            refreshed_slots,
            Some(WriteSlots {
                unconfirmed: 1,
                ..expected_slots
            })
        );

        // Atomic rename with IDENTICAL byte length (same size, changed inode) refreshes dispatches.
        let pre_meta = std::fs::metadata(&reservations).unwrap();
        let same_size_bytes = updated_bytes
            .replacen("\"state\":\"awaiting\"", "\"state\":\"unknown\"", 1)
            .replacen("\"purpose\":\"implement\"", "\"purpose\":\"implement1\"", 1);
        assert_eq!(same_size_bytes.len(), updated_bytes.len());
        let staging = dispatch_dir.join("reservations.json.tmp");
        std::fs::write(&staging, &same_size_bytes).unwrap();
        std::fs::rename(&staging, &reservations).unwrap();
        let post_meta = std::fs::metadata(&reservations).unwrap();
        assert_eq!(post_meta.size(), pre_meta.size());
        assert_ne!(post_meta.ino(), pre_meta.ino());
        let renamed =
            snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache)).unwrap();
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            renamed.report["room_dispatches"]["call-enrolled"]["state"],
            "unknown"
        );

        // Corrupt JSON, wrong schema, and non-object calls fail gracefully to empty room_dispatches.
        for bad in [
            b"".as_slice(),
            b"{corrupt".as_slice(),
            br#"{"schema":2,"calls":{}}"#.as_slice(),
            br#"{"schema":1,"calls":[]}"#.as_slice(),
        ] {
            std::fs::write(&reservations, bad).unwrap();
            let (graceful, slots) = collect(Some("att_main"));
            assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
            assert_eq!(graceful.report["room_dispatches"], serde_json::json!({}));
            assert_eq!(slots, None);
            let cached = cache.0.lock().unwrap();
            assert!(cached.dispatches.as_ref().unwrap().calls.is_none());
        }

        // The Board may read an existing registry larger than the dispatch
        // transaction admits. Keep its Room rows and withhold a gate count.
        let mut padded = raw_bytes.as_bytes().to_vec();
        padded.resize(
            oms_command_telemetry::store::dispatch::MAX_REGISTRY + 1,
            b' ',
        );
        std::fs::write(&reservations, &padded).unwrap();
        let (oversized, oversized_slots) = collect(Some("att_main"));
        assert_eq!(
            oversized.report["room_dispatches"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(oversized_slots, None);

        // Malformed embedded resource or physical contracts withhold capacity while
        // preserving the enrolled schema-1 Room rows without re-querying Events.
        for (key, value) in [
            ("resources", serde_json::json!({"state": "broken"})),
            (
                "physical",
                serde_json::json!({"calls": {"bad": {"schema": 3}}}),
            ),
        ] {
            let mut malformed = full_registry.clone();
            malformed[key] = value;
            std::fs::write(&reservations, serde_json::to_string(&malformed).unwrap()).unwrap();
            let (degraded, degraded_slots) = collect(Some("att_main"));
            assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
            assert_eq!(
                degraded.report["room_dispatches"]
                    .as_object()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(degraded_slots, None);
        }

        // Valid legacy schema-1 (without resources/physical) restores capacity,
        // and symlinked reservations.json fails closed and clears prior cached dispatches.
        std::fs::write(&reservations, &raw_bytes).unwrap();
        let (warm, warm_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(warm.report["room_dispatches"].as_object().unwrap().len(), 2);
        assert_eq!(warm_slots, Some(expected_slots));
        let decoy_file = dispatch_dir.join("decoy.json");
        std::fs::write(&decoy_file, &raw_bytes).unwrap();
        std::fs::remove_file(&reservations).unwrap();
        symlink(&decoy_file, &reservations).unwrap();
        let (sym_file, sym_file_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(sym_file.report["room_dispatches"], serde_json::json!({}));
        assert_eq!(sym_file_slots, None);
        std::fs::remove_file(&reservations).unwrap();

        // Deleted reservations.json clears prior cached dispatches and proves empty capacity.
        std::fs::write(&reservations, &raw_bytes).unwrap();
        assert_eq!(
            snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache))
                .unwrap()
                .report["room_dispatches"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        std::fs::remove_file(&reservations).unwrap();
        let (deleted, deleted_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(deleted.report["room_dispatches"], serde_json::json!({}));
        assert_eq!(
            deleted_slots,
            Some(WriteSlots {
                active: 0,
                awaiting: 8,
                unconfirmed: 0,
            })
        );

        // Symlinked panel-dispatch directory fails closed without reading decoy reservations.json.
        let decoy_dir = repo.join(".oms/artifacts/decoy-dispatch");
        std::fs::create_dir_all(&decoy_dir).unwrap();
        std::fs::write(decoy_dir.join("reservations.json"), &raw_bytes).unwrap();
        std::fs::remove_dir_all(&dispatch_dir).unwrap();
        symlink(&decoy_dir, &dispatch_dir).unwrap();
        let (sym_dir, sym_dir_slots) = collect(Some("att_main"));
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 1);
        assert_eq!(sym_dir.report["room_dispatches"], serde_json::json!({}));
        assert_eq!(sym_dir_slots, None);
        std::fs::remove_file(&dispatch_dir).unwrap();

        // Snapshot error clears cached attempts and dispatches.
        std::fs::create_dir_all(&dispatch_dir).unwrap();
        std::fs::write(&reservations, &raw_bytes).unwrap();
        assert_eq!(
            snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache))
                .unwrap()
                .report["room_dispatches"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        {
            let guard = cache.0.lock().unwrap();
            let cached = guard.dispatches.as_ref().unwrap();
            let cached_map = cached.calls.as_ref().unwrap();
            assert_eq!(cached.generation, dispatch_generation(&repo));
            assert_eq!(
                cached_map.len(),
                full_registry["calls"].as_object().unwrap().len()
            );
            assert!(cached_map["call-enrolled"].get("physical").is_none());
            assert!(
                cached_map["call-enrolled"]["route"]
                    .get("snapshot")
                    .is_none()
            );
        }
        *service.room_error.lock().unwrap() = Some("room unavailable".into());
        assert!(snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache)).is_err());
        {
            let guard = cache.0.lock().unwrap();
            assert!(guard.attempts.is_none());
            assert!(guard.dispatches.is_none());
        }
        *service.room_error.lock().unwrap() = None;
        let recovered =
            snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache)).unwrap();
        assert_eq!(service.events_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            recovered.report["room_dispatches"]
                .as_object()
                .unwrap()
                .len(),
            2
        );

        // Hardlinked reservations.json (nlink != 1) fails closed and clears cached dispatches.
        let hardlink = dispatch_dir.join("hardlink.json");
        std::fs::hard_link(&reservations, &hardlink).unwrap();
        let linked =
            snapshot_with_cache(&repo, Some("room-a"), &service, &stop, Some(&cache)).unwrap();
        assert_eq!(linked.report["room_dispatches"], serde_json::json!({}));
        assert!(cache.0.lock().unwrap().dispatches.is_none());
        let uncached_linked = snapshot(&repo, Some("room-a"), &service, &stop).unwrap();
        assert_eq!(
            uncached_linked.report["room_dispatches"],
            serde_json::json!({})
        );
        std::fs::remove_file(&hardlink).unwrap();

        // Uncached snapshot projects identically and returns empty maps when room_id is None.
        let uncached = snapshot(&repo, Some("room-a"), &service, &stop).unwrap();
        assert_eq!(
            uncached.report["room_dispatches"],
            recovered.report["room_dispatches"]
        );
        let no_room = snapshot(&repo, None, &service, &stop).unwrap();
        assert_eq!(no_room.report["room_attempts"], serde_json::json!({}));
        assert_eq!(no_room.report["room_dispatches"], serde_json::json!({}));

        // Foreign-owned reservations.json leaf withholds capacity while preserving
        // public calls; missing reservations.json (leaf_uid == None) keeps empty capacity.
        let store = Store::open(&repo).unwrap();
        let (cached_attempts, ledger) = cache
            .attempts_with_generation(&repo, &service, &stop)
            .unwrap();
        let ledger = ledger.unwrap();
        let actual_uid = rustix::process::getuid().as_raw();
        let present_read = cache.dispatches(&repo, &store, &stop);
        assert_eq!(
            present_read.generation.as_ref().unwrap().leaf_uid,
            Some(actual_uid)
        );
        assert!(present_read.calls.is_some());
        assert_eq!(
            cache.write_slots_for_uid(
                "att_main",
                &cached_attempts,
                &ledger,
                &present_read,
                actual_uid,
            ),
            Some(expected_slots)
        );
        assert_eq!(
            cache.write_slots_for_uid(
                "att_main",
                &cached_attempts,
                &ledger,
                &present_read,
                actual_uid.wrapping_add(1),
            ),
            None
        );
        assert!(present_read.calls.is_some());

        std::fs::remove_file(&reservations).unwrap();
        let missing_read = cache.dispatches(&repo, &store, &stop);
        assert_eq!(missing_read.generation.as_ref().unwrap().leaf_uid, None);
        assert_eq!(
            cache.write_slots_for_uid(
                "att_main",
                &cached_attempts,
                &ledger,
                &missing_read,
                actual_uid.wrapping_add(1),
            ),
            Some(WriteSlots {
                active: 0,
                awaiting: 8,
                unconfirmed: 0,
            })
        );
    }
}
