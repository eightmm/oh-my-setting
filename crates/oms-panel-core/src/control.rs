//! Shared selection and authority fences. Repository projections grant no scope.
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub(crate) fn scopes<S: Services + ?Sized>(
    repo: &Path,
    worker: &Value,
    service: &S,
    stop: &AtomicBool,
) -> Result<Vec<String>> {
    let ident = text(&worker["refs"], "panel_room_id");
    let member = text(&worker["refs"], "panel_room_participant");
    crate::identifier(&ident)?;
    crate::identifier(&member)?;
    let room = crate::services::query(
        service,
        crate::NativeCommand::Room,
        repo,
        &["show", "--id", &ident, "--json"],
        stop,
    )?;
    let declaration = array(&room["participants"])
        .iter()
        .find(|v| v["participant"] == member)
        .ok_or_else(|| Error::new("worker scope declaration unavailable"))?;
    let values = array(&declaration["owns"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("malformed worker scope"))
        })
        .collect::<Result<Vec<_>>>()?;
    oms_command_room::declared_scopes("worker", &values).map_err(Error::new)?;
    Ok(values)
}

pub(crate) fn lineage<S: Services + ?Sized>(
    repo: &Path,
    attempts: &[Value],
    main_id: &str,
    service: &S,
    stop: &AtomicBool,
    dead: bool,
) -> Result<BTreeSet<String>> {
    let main = attempts
        .iter()
        .find(|v| v["attempt_id"] == main_id)
        .ok_or_else(|| Error::new("owning main absent"))?;
    let member = text(&main["refs"], "panel_room_participant");
    crate::identifier(&member)?;
    let history = attempts
        .iter()
        .filter(|v| {
            (v["tool"] == "panel-main" || v["refs"]["panel_role"] == "main")
                && v["refs"]["panel_room_participant"] == member
        })
        .collect::<Vec<_>>();
    let mut ids = BTreeSet::new();
    require(
        main["tool"] == "panel-main"
            && main["terminal"].as_bool() == Some(dead)
            && history.iter().filter(|v| v["terminal"] == false).count() <= usize::from(!dead),
        "main lineage is unconfirmed",
    )?;
    let mut anchors = BTreeMap::new();
    if history.iter().any(|v| v["provider"] != main["provider"]) {
        let mut provider = Value::Null;
        let rows = service.room_history(repo, &text(&main["refs"], "panel_room_id"), stop)?;
        for row in rows {
            let event = &row["room_event"];
            if event["participant"] != member {
                continue;
            }
            if matches!(event["kind"].as_str(), Some("join" | "main-resume"))
                && event["role"] == "main"
            {
                provider = event["provider"].clone();
            } else if event["kind"] == "main-swap" {
                require(provider.is_string(), "main swap lacks its join anchor")?;
                anchors.insert(text(event, "attempt"), provider);
                provider = event["provider"].clone();
            }
        }
        require(
            provider == main["provider"],
            "current main does not match room swap head",
        )?;
    }
    for row in history {
        let ident = text(row, "attempt_id");
        crate::identifier(&ident)?;
        let terminal = crate::state::TERMINAL_STATES.contains(&text(row, "state").as_str());
        require(
            ids.insert(ident.clone())
                && row["tool"] == "panel-main"
                && row["refs"]["panel_role"] == "main"
                && row["refs"]["panel_owner"]
                    .as_str()
                    .unwrap_or_else(|| row["provider"].as_str().unwrap_or_default())
                    == text(row, "provider")
                && row["terminal"].as_bool() == Some(terminal)
                && (terminal || crate::state::LIVE_STATES.contains(&text(row, "state").as_str())),
            "malformed main lineage",
        )?;
        if row["provider"] != main["provider"] {
            require(
                anchors.get(&ident) == Some(&row["provider"])
                    && terminal
                    && row["state"] == "abandoned"
                    && row["reason_code"] == "main_swapped",
                "foreign main lacks an authenticated room swap",
            )?;
        }
        if terminal {
            require(
                service.host_absent(repo, row)?,
                "previous main host absence is unproven",
            )?;
            crate::release::prove_native_absent(row)?;
        }
    }
    Ok(ids)
}

pub(crate) fn continues(workers: &[Value], start: usize, end: usize) -> bool {
    if start >= end
        || end >= workers.len()
        || workers[start]["task_id"].as_str().is_none_or(str::is_empty)
        || workers[start]["task_id"] != workers[end]["task_id"]
    {
        return false;
    }
    let mut cursor = end;
    while cursor > start {
        let previous = text(&workers[cursor]["refs"], "panel_continues");
        let Some(index) = (start..cursor).rev().find(|i| {
            workers[*i]["attempt_id"] == previous
                && workers[*i]["task_id"] == workers[start]["task_id"]
        }) else {
            return false;
        };
        cursor = index;
    }
    cursor == start
}

pub(crate) fn swap_parents<S: Services + ?Sized>(
    repo: &Path,
    attempts: &[Value],
    main_id: &str,
    service: &S,
    stop: &AtomicBool,
) -> Result<BTreeSet<String>> {
    let eligible = lineage(repo, attempts, main_id, service, stop, false)?;
    if eligible.len() == 1 {
        return Ok(eligible);
    }
    let main = attempts
        .iter()
        .find(|v| v["attempt_id"] == main_id)
        .unwrap();
    let rows = service.room_history(repo, &text(&main["refs"], "panel_room_id"), stop)?;
    let member = text(&main["refs"], "panel_room_participant");
    let mut provider = Value::Null;
    let mut anchors = BTreeMap::new();
    for row in &rows {
        let event = &row["room_event"];
        if event["participant"] != member {
            continue;
        }
        if matches!(event["kind"].as_str(), Some("join" | "main-resume")) && event["role"] == "main"
        {
            provider = event["provider"].clone();
        }
        if event["kind"] == "main-swap" {
            require(
                provider.is_string(),
                "main swap lacks its original provider",
            )?;
            anchors.insert(text(event, "attempt"), provider);
            provider = event["provider"].clone();
        }
    }
    require(
        provider == main["provider"],
        "main provider differs from canonical room swap head",
    )?;
    let mut parents = BTreeSet::from([main_id.to_owned()]);
    for row in attempts
        .iter()
        .filter(|v| eligible.contains(&text(v, "attempt_id")) && v["attempt_id"] != main_id)
    {
        if anchors.get(&text(row, "attempt_id")) == Some(&row["provider"])
            && row["state"] == "abandoned"
            && row["reason_code"] == "main_swapped"
            && (row["refs"]["panel_room_id"] == main["refs"]["panel_room_id"]
                || rows
                    .iter()
                    .any(|r| r["thread"] == row["refs"]["panel_room_id"]))
        {
            parents.insert(text(row, "attempt_id"));
        }
    }
    Ok(parents)
}

pub(crate) struct Selection {
    pub workers: Vec<Value>,
    pub superseded: Vec<Value>,
}
pub(crate) fn select<S: Services + ?Sized>(
    repo: &Path,
    mut workers: Vec<Value>,
    attempts: &[Value],
    rows: &[Value],
    main: Option<&str>,
    service: &S,
    stop: &AtomicBool,
) -> Result<Selection> {
    workers.sort_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")));
    let clean = rows
        .iter()
        .filter(|v| {
            v["kind"] == "delegate"
                && v["exit"] == 0
                && v["patch"].is_string()
                && v["patch_sha256"]
                    .as_str()
                    .is_some_and(|v| crate::hex(v, 64))
        })
        .map(|v| text(v, "attempt_id"))
        .collect::<BTreeSet<_>>();
    let mut selected = Vec::new();
    let mut superseded = Vec::new();
    for (index, worker) in workers.iter().enumerate() {
        let landed = admission(rows, worker).is_some();
        let mut replacement = None;
        if !landed {
            replacement = (index + 1..workers.len())
                .find(|end| continues(&workers, index, *end))
                .map(|end| (end, "continued".to_owned()));
        }
        if replacement.is_none()
            && !landed
            && worker["terminal"] == true
            && ["failed", "timed_out", "cancelled", "abandoned"]
                .contains(&text(worker, "state").as_str())
        {
            for end in index + 1..workers.len() {
                let later = &workers[end];
                let reached = (end..workers.len()).any(|last| {
                    clean.contains(&text(&workers[last], "attempt_id"))
                        && (end == last || continues(&workers, end, last))
                });
                if !(admission(rows, later).is_some() || reached)
                    || later["task_id"] != worker["task_id"]
                    || worker["task_id"].as_str().is_none_or(str::is_empty)
                    || later["refs"]["panel_purpose"] != worker["refs"]["panel_purpose"]
                    || text(later, "created_at") <= text(worker, "created_at")
                {
                    continue;
                }
                let parent = text(later, "parent_attempt_id");
                let old_parent = text(worker, "parent_attempt_id");
                if parent.is_empty() || old_parent.is_empty() {
                    continue;
                }
                if parent != old_parent || main.is_some_and(|v| v != parent) {
                    let Ok(parents) = lineage(
                        repo,
                        attempts,
                        main.unwrap_or(&parent),
                        service,
                        stop,
                        false,
                    ) else {
                        continue;
                    };
                    if !parents.contains(&parent) || !parents.contains(&old_parent) {
                        continue;
                    }
                }
                let old = scopes(repo, worker, service, stop);
                let new = scopes(repo, later, service, stop);
                if let (Ok(old), Ok(new)) = (old, new)
                    && !old.is_empty()
                    && old.iter().all(|v| new.contains(v))
                {
                    replacement = Some((end, text(worker, "reason_code")));
                    break;
                }
            }
        }
        if let Some((end, reason)) = replacement {
            superseded.push(serde_json::json!({"attempt_id":worker["attempt_id"],
                "superseded_by":workers[end]["attempt_id"],"reason":if reason.is_empty() { text(worker,"state") } else { reason }}));
        } else if !crate::salvage::verified_empty(repo, worker, attempts, service, stop)
            .unwrap_or(false)
        {
            selected.push(worker.clone());
        }
    }
    Ok(Selection {
        workers: selected,
        superseded,
    })
}

pub(crate) fn admission<'a>(rows: &'a [Value], worker: &Value) -> Option<&'a Value> {
    let patch = rows.iter().rev().find(|v| {
        v["kind"] == "delegate"
            && v["task_id"] == worker["task_id"]
            && v["attempt_id"] == worker["attempt_id"]
            && v["patch_sha256"].is_string()
    })?;
    if patch["exit"] != 0 {
        return None;
    }
    rows.iter().rev().find(|v| {
        ["patch-land", "panel-accept"].contains(&text(v, "kind").as_str())
            && v["exit"] == 0
            && (v["kind"] == "panel-accept" || v["verify_exit"] == 0)
            && (v["task_id"].is_null() || v["task_id"] == "" || v["task_id"] == worker["task_id"])
            && v["patch_sha256"] == patch["patch_sha256"]
            && (patch["plan_id"].is_null() || v["plan_id"] == patch["plan_id"])
    })
}

pub(crate) fn review_gate(rows: &[Value], task: &str, digest: &Value) -> Result<Value> {
    let row = rows
        .iter()
        .rev()
        .find(|v| {
            v["kind"] == "review-outcome"
                && v["task_id"] == task
                && v["review"]["candidate"]["patch_sha256"] == *digest
        })
        .ok_or_else(|| Error::new("candidate lacks a passing review gate"))?;
    let seats = array(&row["review"]["seats"]);
    require(
        row["exit"] == 0
            && row["review"]["gate_verify_exit"] == 0
            && !seats.is_empty()
            && seats.iter().all(|v| v["verdict"] == "pass"),
        "candidate lacks a passing review gate",
    )?;
    Ok(
        serde_json::json!({"event_id":row["event_id"],"patch_sha256":digest,"exit":row["exit"],
        "gate_verify_exit":row["review"]["gate_verify_exit"],"seat_verdicts":seats.iter().map(|v| &v["verdict"]).collect::<Vec<_>>()}),
    )
}

pub(crate) fn plan_binding(plan: &Value, task: &str) -> Result<Value> {
    let row = &plan["tasks"][task];
    if !row.is_object() {
        return Ok(Value::Null);
    }
    let raw = oms_json::parse_strict(&crate::value::encoded(row)?).map_err(Error::new)?;
    Ok(serde_json::json!({"plan_id":plan["plan_id"],
        "task_sha256":oms_json::projection_cache_digest(&raw).map_err(Error::new)?}))
}

pub(crate) fn python_bytes(value: &Value) -> Result<Vec<u8>> {
    let raw = oms_json::parse_strict(&crate::value::encoded(value)?).map_err(Error::new)?;
    oms_json::canonical_json(&raw).map_err(Error::new)
}

fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.len() == keys.len() && keys.iter().all(|key| v.contains_key(*key)))
}
fn process_shape(value: &Value) -> bool {
    exact_keys(value, &["pid", "identity"])
        && value["pid"]
            .as_u64()
            .is_some_and(|v| v > 0 && v <= i32::MAX as u64)
        && value["identity"].as_str().is_some_and(|v| !v.is_empty())
}
fn valid_boot(value: &Value) -> bool {
    value.as_str().is_some_and(|v| {
        v == "unknown"
            || (v.len() == 36
                && v.split('-').map(str::len).eq([8, 4, 4, 4, 12])
                && v.split('-').all(|v| crate::hex(v, v.len())))
    })
}
pub(crate) fn private_record(repo: &Path, member: &str, suffix: &str) -> Result<Value> {
    crate::identifier(member)?;
    require(
        member.starts_with("call-") && ["launch", "process", "supervisor"].contains(&suffix),
        "invalid private dispatch identity",
    )?;
    let control = crate::terminals::controller_store(repo, false)?;
    let path = control.repo().join(member);
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(&path).map_err(Error::new)?;
    require(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == rustix::process::getuid().as_raw(),
        "unsafe private dispatch directory",
    )?;
    let private = Store::open(&path)?;
    let read = |name: &str| -> Result<Value> {
        let metadata = std::fs::symlink_metadata(path.join(name)).map_err(Error::new)?;
        require(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.mode() & 0o077 == 0
                && metadata.uid() == rustix::process::getuid().as_raw(),
            "unsafe private dispatch file",
        )?;
        private
            .json(Path::new(name), 65536, false)?
            .ok_or_else(|| Error::new("dispatch record unavailable"))
    };
    let identity = read("identity.json")?;
    require(
        exact_keys(
            &identity,
            &[
                "schema",
                "repo",
                "room",
                "participant",
                "parent",
                "task_id",
                "nonce",
                "boot_id",
                "controller",
                "root",
            ],
        ) && identity["schema"].as_u64() == Some(1)
            && identity["repo"]
                == std::fs::canonicalize(repo)
                    .map_err(Error::new)?
                    .to_string_lossy()
                    .as_ref()
            && identity["participant"] == member
            && identity["root"] == path.to_string_lossy().as_ref()
            && valid_boot(&identity["boot_id"])
            && process_shape(&identity["controller"])
            && ["room", "parent", "task_id", "nonce"]
                .iter()
                .all(|key| identity[*key].is_null() || identity[*key].is_string()),
        "invalid dispatch launch identity",
    )?;
    let envelope = read(&format!("{member}-{suffix}.json"))?;
    require(
        exact_keys(
            &envelope,
            &[
                "schema",
                "repo",
                "participant",
                "launch",
                "boot_id",
                "writer",
                "supervisor",
                "record",
            ],
        ) && envelope["schema"].as_u64() == Some(1)
            && envelope["repo"] == identity["repo"]
            && envelope["participant"] == member
            && envelope["launch"] == identity
            && valid_boot(&envelope["boot_id"])
            && process_shape(&envelope["writer"])
            && (envelope["supervisor"].is_null() || process_shape(&envelope["supervisor"])),
        "invalid dispatch controller envelope",
    )?;
    #[cfg(target_os = "linux")]
    {
        let boot =
            std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(Error::new)?;
        require(
            envelope["boot_id"] == boot.trim() && identity["boot_id"] == boot.trim(),
            "dispatch evidence is from another boot",
        )?;
    }
    if suffix == "launch" {
        let call = &envelope["record"]["call"];
        require(
            ["room", "participant", "parent", "task_id", "nonce"]
                .iter()
                .all(|key| call[*key] == identity[*key]),
            "private call differs from launch identity",
        )?;
    }
    Ok(envelope["record"].clone())
}

pub(crate) fn launch(repo: &Path, worker: &Value, parent: &str) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let member = text(&worker["refs"], "panel_room_participant");
    let anchor = private_record(repo, &member, "launch")?;
    let call = &anchor["call"];
    require(
        anchor.as_object().is_some_and(|v| {
            v.keys().all(|key| {
                [
                    "call",
                    "generation",
                    "verify",
                    "execution_repo",
                    "rework",
                    "route",
                ]
                .contains(&key.as_str())
            })
        }) && call.as_object().is_some_and(|v| {
            v.keys().all(|key| {
                [
                    "schema",
                    "kind",
                    "room",
                    "participant",
                    "parent",
                    "nonce",
                    "backend",
                    "task_id",
                    "control_root",
                ]
                .contains(&key.as_str())
            })
        }) && call["schema"].as_u64() == Some(2)
            && call["kind"] == "oms-panel-call"
            && call["participant"] == member
            && call["parent"] == parent
            && call["room"] == worker["refs"]["panel_room_id"]
            && call["task_id"] == worker["task_id"]
            && call["nonce"].as_str().is_some_and(|v| crate::hex(v, 32))
            && ["subreaper", "unsupported"].contains(&text(call, "backend").as_str()),
        "dispatch launch binding changed",
    )?;
    let relative = format!(".oms/artifacts/panel-{member}.json");
    let store = Store::open(repo)?;
    let metadata = std::fs::symlink_metadata(store.repo().join(&relative)).map_err(Error::new)?;
    let generation = serde_json::json!([
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime() as i128 * 1_000_000_000 + metadata.mtime_nsec() as i128,
        metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128
    ]);
    require(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == rustix::process::getuid().as_raw()
            && metadata.mode() & 0o077 == 0
            && anchor["generation"] == generation
            && store.read(Path::new(&relative), 65536, false)? == python_bytes(call)?,
        "public dispatch anchor changed",
    )?;
    Ok(anchor)
}

pub(crate) fn effective_paths(
    repo: &Path,
    task: &Value,
    attempts: &[Value],
) -> Result<Vec<String>> {
    let mut allowed = array(&task["allowed_paths"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new("invalid allowed path"))
        })
        .collect::<Result<Vec<_>>>()?;
    for worker in attempts.iter().filter(|v| {
        v["task_id"] == task["id"]
            && v["refs"]["panel_lease_id"] == task["lease_id"]
            && v["refs"]["panel_role"] == "worker"
            && v["refs"]["panel_access"] == "write"
    }) {
        let Some(parent) = attempts
            .iter()
            .find(|v| v["attempt_id"] == worker["parent_attempt_id"] && v["tool"] == "panel-main")
        else {
            continue;
        };
        let Ok(anchor) = launch(
            repo,
            worker,
            &text(&parent["refs"], "panel_room_participant"),
        ) else {
            continue;
        };
        let context = &anchor["rework"];
        if context.is_null() {
            continue;
        }
        require(
            exact_keys(
                context,
                &["role", "scope", "workload", "brief", "brief_sha256"],
            ) && crate::routing::WRITERS.contains(&text(context, "role").as_str())
                && crate::routing::TIERS.contains(&text(context, "workload").as_str())
                && context["scope"].is_array(),
            "invalid private scope grant",
        )?;
        let brief = format!(
            ".oms/artifacts/panel-{}-brief.md",
            text(&worker["refs"], "panel_room_participant")
        );
        require(
            context["brief"] == brief
                && context["brief_sha256"]
                    == crate::digest(&Store::open(repo)?.read(Path::new(&brief), 65536, false)?),
            "private scope grant brief changed",
        )?;
        for scope in array(&context["scope"]) {
            let scope = scope
                .as_str()
                .ok_or_else(|| Error::new("invalid granted scope"))?;
            require(
                oms_contract_markdown::scope::normalize(scope).map_err(Error::new)? == scope,
                "scope grant is not normalized",
            )?;
            allowed.push(scope.into());
        }
    }
    allowed.sort();
    allowed.dedup();
    Ok(allowed)
}

pub(crate) fn stopped(repo: &Path, worker: &Value, attempts: &[Value]) -> Result<Value> {
    let parent = attempts
        .iter()
        .find(|v| v["attempt_id"] == worker["parent_attempt_id"])
        .ok_or_else(|| Error::new("dispatch parent unavailable"))?;
    let anchor = launch(
        repo,
        worker,
        &text(&parent["refs"], "panel_room_participant"),
    )?;
    let call = &anchor["call"];
    require(
        call["backend"] == "subreaper",
        "dispatch whole-tree supervision unavailable",
    )?;
    let member = text(&worker["refs"], "panel_room_participant");
    let receipt = private_record(repo, &member, "supervisor")?;
    let binding = private_record(repo, &member, "process")?;
    require(
        exact_keys(&receipt, &["launch_sha256", "pid", "all_reaped", "exit"])
            && receipt["pid"]
                .as_u64()
                .is_some_and(|v| v > 0 && v <= i32::MAX as u64)
            && receipt["all_reaped"] == true
            && receipt["exit"].as_i64().is_some()
            && receipt["launch_sha256"] == crate::digest(&python_bytes(call)?)
            && (exact_keys(&binding, &["pid"])
                || (exact_keys(&binding, &["pid", "identity", "group"])
                    && binding["identity"].as_str().is_some_and(|v| !v.is_empty())
                    && binding["group"] == binding["pid"]))
            && binding["pid"] == receipt["pid"],
        "supervisor cleanup or process binding is unproven",
    )?;
    Ok(receipt)
}

/// Uncertain registry entries remain live. A dead PID alone is sufficient only
/// for absence; a reused live PID needs the recorded native generation.
pub(crate) fn review_readers(repo: &Path, execution: &Path) -> Result<()> {
    let relative = Path::new(".oms/locks/canonical-sync.d");
    let store = Store::open(repo)?;
    let path = store.repo().join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::new(e)),
    };
    require(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && std::fs::canonicalize(&path).map_err(Error::new)? == path,
        "unsafe review reader registry",
    )?;
    let entries = std::fs::read_dir(&path).map_err(Error::new)?;
    for entry in entries {
        let entry = entry.map_err(Error::new)?;
        let path = relative.join(entry.file_name());
        let pid = store
            .read(&path.join("pid"), 128, false)
            .ok()
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|v| v.trim().parse::<u32>().ok());
        if let Some(pid) = pid {
            use oms_reference::process::{IdentitySource, NativeIdentity, OwnerError};
            if matches!(NativeIdentity.owner_identity(pid), Err(OwnerError::Exited)) {
                continue;
            }
        }
        if let Ok(source) = store.read(&path.join("source"), 4096, false)
            && let Ok(source) = std::str::from_utf8(&source)
            && let Ok(source) = std::fs::canonicalize(source.trim())
            && source != execution
        {
            continue;
        }
        return Err(Error::busy(
            "review read pass is live or unconfirmed; retry after it finishes",
        ));
    }
    Ok(())
}
