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
#[cfg(target_os = "linux")]
fn valid_linux_boot_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, &byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

/// On Linux, a kernel birth string (`<boot_id>:<starttime>`) from a different
/// boot proves that the process cannot still exist, even if its PID was reused
/// by another UID after reboot.
pub(crate) fn linux_boot_changed(birth: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Some((recorded_boot, start)) = birth.split_once(':') else {
            return false;
        };
        if !valid_linux_boot_id(recorded_boot)
            || start.is_empty()
            || !start.bytes().all(|b| b.is_ascii_digit())
        {
            return false;
        }
        let Ok(current) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
            return false;
        };
        let current = current.trim();
        valid_linux_boot_id(current) && current != recorded_boot
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = birth;
        false
    }
}

pub(crate) fn boot_changed(refs: &Value, kind: &str) -> bool {
    let birth = refs[format!("panel_{kind}_identity")]
        .as_str()
        .unwrap_or_default();
    linux_boot_changed(birth)
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
    if linux_boot_changed(birth) {
        return true;
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
    release_locked(repo, attempt, &start, &session, service, stop)
}

/// Release a provably absent panel-main while the caller already holds the
/// `panel-main-start` and `panel-main-session` transactions.
pub fn release_locked(
    repo: &Path,
    attempt: &str,
    start: &crate::Transaction<'_>,
    session: &crate::Transaction<'_>,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    crate::identifier(attempt)?;
    start.check()?;
    session.check()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{CommandOutput, ExecutionRequest, PeerExecutor};
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "oms-release-unit-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct FakeReleaseServices {
        row: RefCell<Value>,
    }
    impl PeerExecutor for FakeReleaseServices {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            Err(Error::new("tmux unavailable after reboot"))
        }
    }
    impl Services for FakeReleaseServices {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            args: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            assert_eq!(command, NativeCommand::Events);
            if args.first().and_then(|v| v.to_str()) == Some("transition") {
                let mut row = self.row.borrow_mut();
                row["state"] = "abandoned".into();
                row["terminal"] = true.into();
                row["reason_code"] = REASON.into();
            }
            Ok(CommandOutput {
                stdout: serde_json::to_vec(&*self.row.borrow()).unwrap(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            })
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reboot_proves_process_and_tmux_host_absence_even_if_pid_reused() {
        let current = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .to_owned();
        let other = if current.starts_with('0') {
            "11111111-1111-4111-8111-111111111111"
        } else {
            "00000000-0000-4000-8000-000000000000"
        };
        assert!(linux_boot_changed(&format!("{other}:12345")));
        assert!(!linux_boot_changed(&format!("{current}:12345")));
        assert!(!linux_boot_changed("synthetic:12345"));
        assert!(!linux_boot_changed("darwin:123:456"));

        // PID 1 is owned by root (foreign UID); boot_id mismatch still proves absence.
        let rebooted_refs = serde_json::json!({
            "panel_host": "tmux",
            "panel_pane_id": "%1",
            "panel_socket_digest": "a".repeat(64),
            "panel_tmux_server_pid": "1",
            "panel_tmux_server_identity": format!("{other}:42"),
            "panel_wrapper_pid": "1",
            "panel_wrapper_identity": format!("{other}:43"),
            "panel_native_pid": "1",
            "panel_native_identity": format!("{other}:44"),
        });
        assert!(identity_gone(&rebooted_refs, "tmux_server"));
        assert!(identity_gone(&rebooted_refs, "wrapper"));
        assert!(identity_gone(&rebooted_refs, "native"));

        let dir = TempDir::new();
        let row = serde_json::json!({
            "attempt_id": "att_11111111111111111111111111111111",
            "tool": "panel-main",
            "provider": "codex",
            "state": "working",
            "terminal": false,
            "created_at": "2020-01-01T00:00:00Z",
            "last_heartbeat_at": "2020-01-01T00:00:00Z",
            "refs": {
                "panel_role": "main",
                "panel_host": "tmux",
                "panel_pane_id": "%1",
                "panel_socket_digest": "a".repeat(64),
                "panel_tmux_server_pid": "1",
                "panel_tmux_server_identity": format!("{other}:42"),
                "panel_wrapper_pid": "1",
                "panel_wrapper_identity": format!("{other}:43"),
                "panel_native_pid": "1",
                "panel_native_identity": format!("{other}:44"),
            }
        });
        let svc = FakeReleaseServices {
            row: RefCell::new(row.clone()),
        };
        assert!(crate::host::absent(&dir.0, &row, &svc).unwrap());

        // Pre-fix tmux attempts lacking panel_pane_id still release after a Linux reboot,
        // but stay unverifiable on the same boot.
        let mut legacy_tmux = row.clone();
        legacy_tmux["refs"]
            .as_object_mut()
            .unwrap()
            .remove("panel_pane_id");
        legacy_tmux["refs"]
            .as_object_mut()
            .unwrap()
            .remove("panel_socket_digest");
        assert!(crate::host::absent(&dir.0, &legacy_tmux, &svc).unwrap());
        legacy_tmux["refs"]["panel_wrapper_identity"] = format!("{current}:43").into();
        assert!(!crate::host::absent(&dir.0, &legacy_tmux, &svc).unwrap());

        // Releasing with already-held start and session locks succeeds without deadlocking.
        let store = Store::open(&dir.0).unwrap();
        let start = store
            .transaction("panel-main-start", Duration::from_secs(1))
            .unwrap();
        let session = store
            .transaction("panel-main-session", Duration::from_secs(1))
            .unwrap();
        let stop = AtomicBool::new(false);
        let released = release_locked(
            &dir.0,
            "att_11111111111111111111111111111111",
            &start,
            &session,
            &svc,
            &stop,
        )
        .unwrap();
        assert_eq!(released["state"], "abandoned");
        assert_eq!(svc.row.borrow()["terminal"], true);
    }
}
