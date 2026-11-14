//! Explicit release requires positive wrapper, native and host absence proofs.
use crate::services::{NativeCommand, query};
use crate::{Error, Result, Services, Store, Value, require};
use oms_reference::process::{IdentitySource, NativeIdentity, OwnerError};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
pub const REASON: &str = "panel_main_released";
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
    matches!(NativeIdentity.owner_identity(pid), Err(OwnerError::Exited))
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
    let _start = store.transaction("panel-main-start", Duration::from_secs(3))?;
    let _session = store.transaction("panel-main-session", Duration::from_secs(3))?;
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
