//! Explicit release requires positive wrapper, native and host absence proofs.
use crate::services::{NativeCommand, query};
use crate::{Error, Result, Services, Store, Value, require};
use oms_reference::process::{IdentitySource, NativeIdentity, OwnerError};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const REASON: &str = "panel_main_released";
#[derive(Clone, Debug)]
pub struct MainSwapRequest {
    pub attempt_id: String,
    pub provider: String,
    pub model: String,
    pub harness: String,
}
/// Swap orchestration requires an injected native host handshake. A failed or
/// interrupted handshake is uncertain; retry requires host reconciliation.
pub fn swap_main(
    repo: &Path,
    request: &MainSwapRequest,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    swap_main_in_room(repo, request, None, service, stop)
}

pub fn swap_main_in_room(
    repo: &Path,
    request: &MainSwapRequest,
    expected_room: Option<&str>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(&request.attempt_id)?;
    require(
        ["codex", "claude", "pi", "antigravity"].contains(&request.provider.as_str())
            && ["codex", "claude", "pi", "agy"].contains(&request.harness.as_str())
            && oms_command_room::main_owner_matches(&request.harness, &request.provider)
            && !request.model.is_empty()
            && request.model.len() <= 160
            && request
                .model
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"_./:-".contains(&v)),
        "invalid main swap target",
    )?;
    let store = Store::open(repo)?;
    // The live wrapper holds panel-main-session for its full lifetime. The
    // startup reservation serializes swaps without waiting for that lock before
    // asking the source to leave.
    let start = store.transaction("panel-main-start", Duration::from_secs(3))?;
    let attempts = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let rows = crate::value::array(&attempts);
    let main = rows
        .iter()
        .find(|v| v["attempt_id"] == request.attempt_id)
        .ok_or_else(|| Error::new("main swap source unavailable"))?;
    let member = crate::value::text(&main["refs"], "panel_room_participant");
    let room = crate::value::text(&main["refs"], "panel_room_id");
    require(
        expected_room.is_none_or(|expected| expected == room),
        "Main swap source belongs to a different Room",
    )?;
    require(
        main["tool"] == "panel-main"
            && main["terminal"] == false
            && main["state"] == "working"
            && main["refs"]["panel_host"] == "tmux"
            && crate::identifier(&member).is_ok()
            && crate::identifier(&room).is_ok()
            && !(main["refs"]["panel_harness"] == request.harness
                && main["refs"]["panel_model"] == request.model)
            && rows
                .iter()
                .filter(|v| {
                    v["tool"] == "panel-main"
                        && v["terminal"] == false
                        && v["refs"]["panel_room_id"] == room
                })
                .count()
                == 1,
        "main swap requires one live owned tmux main and a different target",
    )?;
    let before = query(
        service,
        NativeCommand::Room,
        repo,
        &["show", "--id", &room, "--exact", "--json"],
        stop,
    )?;
    require(
        before["id"] == room
            && before["closed"] == false
            && before["successor"].is_null()
            && crate::value::array(&before["participants"])
                .iter()
                .any(|v| {
                    v["participant"] == member
                        && v["joined"] == true
                        && v["role"] == "main"
                        && v["provider"] == main["provider"]
                }),
        "main swap room ownership changed",
    )?;
    start.check()?;
    let receipt = service
        .swap_main_host(repo, request, main, stop)
        .map_err(|e| Error {
            publication_uncertain: true,
            ..e
        })?;
    (|| -> Result<Value> {
        start.check()?;
        let attempts = query(
            service,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        let rows = crate::value::array(&attempts);
        let prior = rows
            .iter()
            .find(|v| v["attempt_id"] == request.attempt_id)
            .ok_or_else(|| Error::new("swapped main evidence unavailable"))?;
        require(
            prior["terminal"] == true
                && prior["state"] == "abandoned"
                && prior["reason_code"] == "main_swapped",
            "old main exit is unconfirmed",
        )?;
        prove_native_absent(prior)?;
        let replacement = rows
            .iter()
            .filter(|v| {
                v["tool"] == "panel-main"
                    && v["terminal"] == false
                    && v["refs"]["panel_room_participant"] == member
            })
            .collect::<Vec<_>>();
        require(
            replacement.len() == 1
                && replacement[0]["attempt_id"] != request.attempt_id
                && replacement[0]["provider"] == request.provider
                && replacement[0]["refs"]["panel_harness"] == request.harness
                && replacement[0]["refs"]["panel_model"] == request.model,
            "replacement main startup is unconfirmed",
        )?;
        let parents = crate::control::swap_parents(
            repo,
            rows,
            replacement[0]["attempt_id"].as_str().unwrap_or_default(),
            service,
            stop,
        )?;
        require(
            parents.contains(&request.attempt_id),
            "main swap lacks canonical room lineage",
        )?;
        Ok(receipt)
    })()
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })
}
/// Legacy identities have a different encoding: only proven PID absence is
/// comparable here. A live but differently encoded generation remains unknown.
pub fn identity_gone(refs: &Value, kind: &str) -> bool {
    let raw = refs[format!("panel_{kind}_pid")]
        .as_str()
        .unwrap_or_default();
    let birth = refs[format!("panel_{kind}_identity")]
        .as_str()
        .unwrap_or_default();
    if raw.is_empty() || birth.is_empty() || !raw.bytes().all(|v| v.is_ascii_digit()) {
        return false;
    }
    let Ok(pid) = raw.parse::<u32>() else {
        return false;
    };
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    match NativeIdentity.owner_identity(pid) {
        Err(OwnerError::Exited) => true,
        #[cfg(target_os = "linux")]
        Ok(current) if crate::hex(birth, 64) => crate::digest(current.birth.as_bytes()) != birth,
        Ok(current) if birth.contains(':') => current.birth != birth,
        _ => false,
    }
}
pub fn heartbeat_fresh(row: &Value, now: f64) -> Result<bool> {
    let refs = &row["refs"];
    let read = |name, default| {
        refs[name]
            .as_str()
            .unwrap_or(default)
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 0 && *v <= 999999)
    };
    let interval = read("panel_heartbeat_interval", "60")
        .ok_or_else(|| Error::new("heartbeat interval unavailable"))?;
    let ttl = read("panel_heartbeat_ttl", "300")
        .ok_or_else(|| Error::new("heartbeat TTL unavailable"))?;
    require(ttl >= interval, "heartbeat bounds unavailable")?;
    let created = crate::metrics::stamp(&row["created_at"])
        .ok_or_else(|| Error::new("creation evidence unavailable"))?;
    let heartbeat = crate::metrics::stamp(&row["last_heartbeat_at"])
        .ok_or_else(|| Error::new("heartbeat evidence unavailable"))?;
    Ok(now - created < interval as f64 || now - heartbeat < ttl as f64)
}
pub fn prove_native_absent(row: &Value) -> Result<()> {
    let refs = &row["refs"];
    if refs.get("panel_wrapper_pid").is_some() || refs.get("panel_wrapper_identity").is_some() {
        require(
            identity_gone(refs, "wrapper"),
            "wrapper termination is unconfirmed",
        )?;
    }
    if identity_gone(refs, "native") {
        return Ok(());
    }
    let code = refs["panel_native_exit_code"]
        .as_str()
        .and_then(|v| v.parse::<i32>().ok());
    require(
        row["reason_code"] != "native_shutdown_unconfirmed"
            && refs["panel_native_exit"] == "confirmed"
            && code.is_some(),
        "native termination is unconfirmed",
    )
}
pub fn release(
    repo: &Path,
    attempt: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(attempt)?;
    let store = Store::open(repo)?;
    let start = store.transaction("panel-main-start", Duration::from_secs(3))?;
    let session = store.transaction("panel-main-session", Duration::from_secs(3))?;
    let row = query(
        service,
        NativeCommand::Events,
        repo,
        &["show", "--attempt", attempt, "--json"],
        stop,
    )?;
    require(
        row["attempt_id"] == attempt
            && row["tool"] == "panel-main"
            && row["refs"]["panel_role"] == "main"
            && ["working", "blocked"].contains(&row["state"].as_str().unwrap_or_default()),
        "release requires a panel-main in working or blocked state",
    )?;
    require(
        !heartbeat_fresh(&row, crate::timestamp() as f64)?
            || identity_gone(&row["refs"], "wrapper"),
        "main heartbeat is fresh or pending",
    )?;
    let host_absent = if row["refs"]["panel_host"] == "inline" {
        identity_gone(&row["refs"], "wrapper")
    } else {
        service.host_absent(repo, &row)?
    };
    require(host_absent, "main host absence is unproven")?;
    prove_native_absent(&row)?;
    start.check()?;
    session.check()?;
    let mut argv = crate::services::args(&["transition", "--attempt", attempt, "--state"]);
    if row["state"] == "working" {
        argv.extend(crate::services::args(&["blocked", "--then", "abandoned"]));
    } else {
        argv.push("abandoned".into());
    }
    argv.extend(crate::services::args(&[
        "--actor",
        "panel-main-release",
        "--reason-code",
        REASON,
    ]));
    service
        .command(NativeCommand::Events, repo, &argv, stop)?
        .success()?;
    let final_row = query(
        service,
        NativeCommand::Events,
        repo,
        &["show", "--attempt", attempt, "--json"],
        stop,
    )?;
    require(
        final_row["state"] == "abandoned" && final_row["terminal"] == true,
        "main release could not be verified; inspect lifecycle state",
    )?;
    Ok(
        serde_json::json!({"attempt":attempt,"previous_state":row["state"],"state":"abandoned","reason":REASON}),
    )
}
pub fn classify(repo: &Path, attempts: &[Value], service: &dyn Services) -> Value {
    let mut releasable = vec![];
    let mut unverifiable = vec![];
    for row in attempts
        .iter()
        .filter(|v| v["tool"] == "panel-main" && v["terminal"] == false)
        .take(16)
    {
        let eligible = heartbeat_fresh(row, crate::timestamp() as f64)
            .is_ok_and(|fresh| !fresh || identity_gone(&row["refs"], "wrapper"))
            && prove_native_absent(row).is_ok()
            && if row["refs"]["panel_host"] == "inline" {
                identity_gone(&row["refs"], "wrapper")
            } else {
                service.host_absent(repo, row).unwrap_or(false)
            };
        let item = serde_json::json!({"attempt":row["attempt_id"],"provider":row["provider"],"state":row["state"]});
        if eligible {
            releasable.push(item);
        } else {
            unverifiable.push(item);
        }
    }
    serde_json::json!({"releasable":releasable,"unverifiable":unverifiable})
}
