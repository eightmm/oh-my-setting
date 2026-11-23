//! Immutable native assignment custody across a canonical Main swap.
//!
//! A retained record proves a binding, not live authority. Mutation callers must
//! also prove the canonical swap lineage and freshly sample the successor host.
use crate::native_contract::CheckedAssignment;
use crate::value::{array, text};
use crate::{Error, Result, Services, Store, Value, require};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const MAX_SUCCESSION: usize = 16 * 1024;

#[derive(Clone)]
pub struct CheckedSuccession {
    value: Value,
    sha256: String,
}

fn exact_fields(value: &Value, fields: &[&str]) -> Result<()> {
    require(
        value.as_object().is_some_and(|object| {
            object.len() == fields.len() && fields.iter().all(|key| object.contains_key(*key))
        }),
        "native succession has missing or unknown fields",
    )
}

fn sha(value: &Value) -> Result<&str> {
    let digest = value.as_str().unwrap_or_default();
    require(crate::hex(digest, 64), "invalid native succession digest")?;
    Ok(digest)
}

fn artifact(path: &str, attempt: &str) -> Result<()> {
    require(
        Path::new(path).parent() == Some(Path::new(&format!(".oms/lifecycle/attempts/{attempt}")))
            && Path::new(path).file_name().is_some()
            && !path.contains(['\\', '\0'])
            && !path.contains("//")
            && !path.split('/').any(|part| matches!(part, "." | ".."))
            && !path.chars().any(char::is_control)
            && path.len() <= 512,
        "native succession artifact leaves its assignment directory",
    )
}

impl CheckedSuccession {
    pub fn parse_bound(raw: &[u8], assignment: &CheckedAssignment) -> Result<Self> {
        let value = crate::decode(raw, MAX_SUCCESSION)?;
        exact_fields(
            &value,
            &[
                "schema",
                "kind",
                "mode",
                "attempt_id",
                "parent_attempt_id",
                "room",
                "task_sha256",
                "checkout_sha256",
                "assignment",
                "result_sha256",
                "previous",
                "custody",
            ],
        )?;
        require(
            value["schema"] == 1
                && value["kind"] == "oms-native-main-succession"
                && matches!(value["mode"].as_str(), Some("continuation" | "retirement"))
                && value["attempt_id"] == assignment.attempt_id()
                && value["parent_attempt_id"] == assignment.parent_attempt_id()
                && value["room"] == assignment.room()
                && value["task_sha256"] == assignment.task_sha256()
                && value["checkout_sha256"] == assignment.checkout_sha256(),
            "native succession changes the frozen assignment identity",
        )?;
        exact_fields(&value["assignment"], &["path", "sha256", "round"])?;
        artifact(&text(&value["assignment"], "path"), assignment.attempt_id())?;
        sha(&value["assignment"]["sha256"])?;
        require(
            value["assignment"]["round"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= assignment.round()),
            "native succession round is invalid",
        )?;
        if !value["result_sha256"].is_null() {
            sha(&value["result_sha256"])?;
        }
        if !value["previous"].is_null() {
            exact_fields(&value["previous"], &["sha256", "main_attempt_id"])?;
            sha(&value["previous"]["sha256"])?;
            crate::identifier(&text(&value["previous"], "main_attempt_id"))?;
        }
        let custody = &value["custody"];
        exact_fields(
            custody,
            &[
                "main_attempt_id",
                "main_owner",
                "participant",
                "session",
                "main_pane",
                "lead_pane",
                "account",
                "account_verification",
                "checkout",
            ],
        )?;
        for key in ["main_attempt_id", "main_owner", "participant"] {
            crate::identifier(&text(custody, key))?;
        }
        require(
            custody["main_attempt_id"] != assignment.parent_attempt_id()
                && custody["main_attempt_id"] != value["previous"]["main_attempt_id"]
                && custody["participant"] == assignment.parent_participant()
                && custody["session"].is_object()
                && custody["main_pane"].is_object()
                && custody["lead_pane"] == assignment.packet()["lead_pane"]
                && custody["account"] == assignment.packet()["account"]
                && custody["account_verification"] == assignment.packet()["account_verification"]
                && custody["checkout"] == *assignment.checkout(),
            "native succession changes Lead, account or checkout custody",
        )?;
        // All coordinates are sampled by the producer; malformed records remain
        // unavailable even in the read-only projection.
        exact_fields(&custody["main_pane"], &["id", "window", "pid", "birth"])?;
        exact_fields(
            &custody["session"],
            &["session", "server_pid", "server_birth", "socket_digest"],
        )?;
        require(
            custody["session"]["session"].as_str().is_some_and(|s| {
                s.strip_prefix('$').is_some_and(|v| {
                    !v.is_empty() && v.len() <= 32 && v.bytes().all(|c| c.is_ascii_digit())
                })
            }) && custody["session"]["server_pid"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= u32::MAX as u64)
                && custody["session"]["server_birth"]
                    .as_str()
                    .is_some_and(|s| {
                        !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
                    })
                && crate::hex(
                    custody["session"]["socket_digest"]
                        .as_str()
                        .unwrap_or_default(),
                    64,
                ),
            "invalid native successor session identity",
        )?;
        require(
            custody["main_pane"]["id"].as_str().is_some_and(|s| {
                s.strip_prefix('%').is_some_and(|v| {
                    !v.is_empty() && v.len() <= 32 && v.bytes().all(|c| c.is_ascii_digit())
                })
            }) && custody["main_pane"]["window"].as_str().is_some_and(|s| {
                s.strip_prefix('@').is_some_and(|v| {
                    !v.is_empty() && v.len() <= 32 && v.bytes().all(|c| c.is_ascii_digit())
                })
            }) && custody["main_pane"]["pid"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= u32::MAX as u64)
                && custody["main_pane"]["birth"].as_str().is_some_and(|s| {
                    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
                }),
            "invalid native successor Main identity",
        )?;
        let digest = crate::digest(raw);
        require(
            value["assignment"]["sha256"] == assignment.sha256()
                || (value["assignment"]["round"].as_u64().unwrap() < assignment.round()
                    && assignment.packet()["custody_succession_sha256"] == digest),
            "native round does not carry its exact successor custody",
        )?;
        Ok(Self {
            value,
            sha256: digest,
        })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn main_attempt_id(&self) -> &str {
        self.value["custody"]["main_attempt_id"].as_str().unwrap()
    }
    pub fn main_owner(&self) -> &str {
        self.value["custody"]["main_owner"].as_str().unwrap()
    }
    pub fn participant(&self) -> &str {
        self.value["custody"]["participant"].as_str().unwrap()
    }
    pub fn custody(&self) -> &Value {
        &self.value["custody"]
    }
    pub fn retirement_only(&self) -> bool {
        self.value["mode"] == "retirement"
    }
    pub fn previous_main(&self) -> &str {
        self.value["previous"]["main_attempt_id"]
            .as_str()
            .unwrap_or_else(|| self.value["parent_attempt_id"].as_str().unwrap())
    }
}

/// Reads only the bounded custody record and the assignment it froze. A later
/// round must explicitly carry the same custody digest; old packets stay intact.
pub fn read_bound(
    store: &Store,
    operation: &Value,
    assignment: &CheckedAssignment,
) -> Result<Option<CheckedSuccession>> {
    let refs = &operation["refs"];
    let path = text(refs, "native_succession_path");
    let digest = text(refs, "native_succession_sha256");
    let main = text(refs, "native_custodian_attempt");
    if [
        "native_succession_schema",
        "native_succession_path",
        "native_succession_sha256",
        "native_custodian_attempt",
    ]
    .iter()
    .all(|key| refs.get(key).is_none())
    {
        return Ok(None);
    }
    artifact(&path, assignment.attempt_id())?;
    require(
        crate::hex(&digest, 64) && !main.is_empty() && refs["native_succession_schema"] == "1",
        "partial native succession reference",
    )?;
    require(
        path == format!(
            ".oms/lifecycle/attempts/{}/native-succession-{digest}.json",
            assignment.attempt_id()
        ),
        "native succession reference has a noncanonical artifact path",
    )?;
    let raw = store.read(Path::new(&path), MAX_SUCCESSION, false)?;
    require(
        crate::digest(&raw) == digest,
        "native succession digest changed",
    )?;
    let checked = CheckedSuccession::parse_bound(&raw, assignment)?;
    require(
        checked.main_attempt_id() == main,
        "native successor reference changed",
    )?;
    let anchor = &checked.value()["assignment"];
    let frozen = store.read(
        Path::new(&text(anchor, "path")),
        crate::native_contract::MAX_ASSIGNMENT,
        false,
    )?;
    let frozen = CheckedAssignment::parse(&frozen)?
        .ok_or_else(|| Error::new("native succession requires a typed assignment"))?;
    require(
        frozen.sha256() == text(anchor, "sha256")
            && frozen.round() == anchor["round"]
            && frozen.attempt_id() == assignment.attempt_id()
            && frozen.parent_attempt_id() == assignment.parent_attempt_id()
            && frozen.room() == assignment.room()
            && frozen.task_sha256() == assignment.task_sha256()
            && frozen.checkout_sha256() == assignment.checkout_sha256()
            && frozen.packet()["lead_pane"] == assignment.packet()["lead_pane"]
            && frozen.packet()["account"] == assignment.packet()["account"]
            && frozen.packet()["account_verification"]
                == assignment.packet()["account_verification"],
        "native succession anchor or frozen execution contract changed",
    )?;
    let mut cursor = checked.clone();
    let mut anchor = frozen;
    let mut seen = std::collections::BTreeSet::from([checked.sha256().to_owned()]);
    let mut mains = std::collections::BTreeSet::from([checked.main_attempt_id().to_owned()]);
    while !cursor.value()["previous"].is_null() {
        require(
            seen.len() < 32,
            "native succession chain exceeds the bounded history",
        )?;
        let previous = &cursor.value()["previous"];
        let digest = sha(&previous["sha256"])?;
        require(
            seen.insert(digest.to_owned()),
            "native succession chain repeats a record",
        )?;
        let path = format!(
            ".oms/lifecycle/attempts/{}/native-succession-{digest}.json",
            assignment.attempt_id()
        );
        let raw = store.read(Path::new(&path), MAX_SUCCESSION, false)?;
        require(
            crate::digest(&raw) == digest,
            "native succession predecessor digest changed",
        )?;
        let value = crate::decode(&raw, MAX_SUCCESSION)?;
        let old_path = text(&value["assignment"], "path");
        artifact(&old_path, assignment.attempt_id())?;
        let old_raw = store.read(
            Path::new(&old_path),
            crate::native_contract::MAX_ASSIGNMENT,
            false,
        )?;
        let old_assignment = CheckedAssignment::parse(&old_raw)?
            .ok_or_else(|| Error::new("native predecessor has no typed assignment"))?;
        let old = CheckedSuccession::parse_bound(&raw, &old_assignment)?;
        require(
            mains.insert(old.main_attempt_id().to_owned())
                && old.main_attempt_id() == previous["main_attempt_id"]
                && old.value()["assignment"]["sha256"] == old_assignment.sha256()
                && old_assignment.task_sha256() == assignment.task_sha256()
                && old_assignment.parent_attempt_id() == assignment.parent_attempt_id()
                && old_assignment.room() == assignment.room()
                && old_assignment.checkout_sha256() == assignment.checkout_sha256()
                && old_assignment.round() <= anchor.round()
                && (old_assignment.sha256() == anchor.sha256()
                    || anchor.packet()["custody_succession_sha256"] == old.sha256()),
            "native succession predecessor does not match its immutable assignment chain",
        )?;
        cursor = old;
        anchor = old_assignment;
    }
    Ok(Some(checked))
}

/// Read-only observation for Results and waiters. It never grants owner authority.
pub fn metadata(store: &Store, operation: &Value) -> Result<Option<Value>> {
    if operation["refs"].get("native_succession_schema").is_none()
        && operation["refs"].get("native_succession_path").is_none()
        && operation["refs"].get("native_succession_sha256").is_none()
        && operation["refs"].get("native_custodian_attempt").is_none()
    {
        return Ok(None);
    }
    let path = text(&operation["refs"], "native_binding");
    artifact(&path, &text(operation, "attempt_id"))?;
    let raw = store.read(
        Path::new(&path),
        crate::native_contract::MAX_ASSIGNMENT,
        false,
    )?;
    require(
        crate::digest(&raw) == operation["refs"]["native_binding_sha256"],
        "native assignment digest changed",
    )?;
    let assignment = CheckedAssignment::parse(&raw)?
        .ok_or_else(|| Error::new("native succession requires a typed assignment"))?;
    Ok(read_bound(store, operation, &assignment)?.map(|record| serde_json::json!({
        "schema":1,"status":"retained","sha256":record.sha256(),
        "attempt_id":assignment.attempt_id(),"parent_attempt_id":assignment.parent_attempt_id(),
        "main_attempt_id":record.main_attempt_id(),"main_owner":record.main_owner(),
        "mode":record.value()["mode"],
        "previous_main_attempt_id":record.previous_main(),
        "session":record.custody()["session"],"main_pane":record.custody()["main_pane"],
        "assignment_sha256":assignment.sha256(),
        "participant":record.participant(),"room":assignment.room(),
        "task_id":assignment.task_id(),"plan_id":assignment.plan_id(),"lease_id":assignment.lease_id(),
        "live_authority":"requires-revalidation"
    })))
}

/// Existing Room main-swap history and host/process absence are the only source
/// of successor authority. Same Room, elapsed time, or a saved record is not proof.
#[allow(clippy::too_many_arguments)]
pub fn prove_successor(
    repo: &Path,
    attempts: &[Value],
    original: &str,
    previous: &str,
    current: &str,
    room: &str,
    participant: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    require(
        original != current && previous != current,
        "native succession needs a different Main",
    )?;
    let main = attempts
        .iter()
        .find(|row| row["attempt_id"] == current)
        .ok_or_else(|| Error::new("native successor Main is absent"))?;
    require(
        main["tool"] == "panel-main"
            && main["terminal"] == false
            && crate::state::LIVE_STATES.contains(&text(main, "state").as_str())
            && main["refs"]["panel_role"] == "main"
            && main["refs"]["panel_room_id"] == room
            && main["refs"]["panel_room_participant"] == participant
            && main["refs"]["panel_native_scheduling"] == "unmanaged"
            && attempts
                .iter()
                .filter(|row| {
                    row["tool"] == "panel-main"
                        && row["refs"]["panel_room_id"] == room
                        && row["terminal"] != true
                })
                .count()
                == 1,
        "native successor Room or scheduling ownership changed",
    )?;
    let room_view = crate::services::query(
        service,
        crate::NativeCommand::Room,
        repo,
        &["show", "--id", room, "--exact", "--json"],
        stop,
    )?;
    let swaps = room_view["swaps"]
        .as_array()
        .ok_or_else(|| Error::new("canonical Main swap history unavailable"))?;
    let start = swaps
        .iter()
        .position(|swap| swap["attempt"] == original)
        .ok_or_else(|| Error::new("original Main lacks a canonical swap"))?;
    let chain = &swaps[start..];
    require(
        chain.len() <= 32 && chain.iter().any(|swap| swap["attempt"] == previous),
        "native successor chain is too long or has an out-of-order predecessor",
    )?;
    let mut seen = std::collections::BTreeSet::new();
    for (index, swap) in chain.iter().enumerate() {
        let id = text(swap, "attempt");
        require(
            seen.insert(id.clone()) && swap["participant"] == participant,
            "native successor chain is repeated or belongs to another participant",
        )?;
        let old = attempts
            .iter()
            .find(|row| row["attempt_id"] == id)
            .ok_or_else(|| Error::new("previous native Main is absent from history"))?;
        require(
            old["tool"] == "panel-main"
                && old["terminal"] == true
                && old["state"] == "abandoned"
                && old["reason_code"] == "main_swapped"
                && old["provider"] == swap["from"]
                && old["refs"]["panel_room_id"] == room
                && crate::state::main_participant(old) == participant
                && (index == 0
                    || (old["provider"] == chain[index - 1]["to"]
                        && old["refs"]["panel_harness"] == chain[index - 1]["harness"]
                        && old["refs"]["panel_model"] == chain[index - 1]["model"]))
                && old["refs"]["panel_native_scheduling"] == "unmanaged"
                && [
                    "panel_wrapper_pid",
                    "panel_wrapper_identity",
                    "panel_native_pid",
                    "panel_native_identity",
                ]
                .iter()
                .all(|key| old["refs"][key].as_str().is_some_and(|s| !s.is_empty())),
            "previous Main has no exact completed native swap",
        )?;
        // A canonical native swap reuses the tmux pane. Process generations,
        // not pane absence, prove the outgoing wrapper and provider are gone.
        require(
            crate::release::identity_gone(&old["refs"], "wrapper")
                && crate::release::identity_gone(&old["refs"], "native"),
            "previous Main process absence is unproven",
        )?;
        crate::release::prove_native_absent(old)?;
    }
    let latest = chain.last().unwrap();
    require(
        main["provider"] == latest["to"]
            && main["refs"]["panel_harness"] == latest["harness"]
            && main["refs"]["panel_model"] == latest["model"]
            && main["refs"]["panel_owner"] == main["provider"],
        "native successor differs from the latest canonical swap target",
    )?;
    require(
        room_view["id"] == room
            && room_view["closed"] == false
            && array(&room_view["participants"])
                .iter()
                .filter(|row| row["joined"] == true && row["role"] == "main")
                .count()
                == 1
            && array(&room_view["participants"]).iter().any(|row| {
                row["participant"] == participant
                    && row["joined"] == true
                    && row["role"] == "main"
                    && row["provider"] == main["provider"]
            }),
        "native successor is not the exact current Room Main",
    )
}
