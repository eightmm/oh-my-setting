//! Review/council metadata and resumable settlement claims.
use crate::{Error, Result, Store, Value, require};
use std::path::Path;
use std::time::Duration;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub patch: String,
    pub sha256: String,
    pub base: String,
}
impl Candidate {
    pub fn validate(&self) -> Result<()> {
        crate::storage::path(Path::new(&self.patch))?;
        require(
            self.patch.starts_with(".oms/artifacts/")
                && crate::hex(&self.sha256, 64)
                && (7..=64).contains(&self.base.len())
                && self
                    .base
                    .bytes()
                    .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase()),
            "automatic review requires linked patch digest and recorded base",
        )
    }
}
pub fn verdict(raw: &Value) -> Result<&str> {
    let verdict = raw["verdict"]
        .as_str()
        .ok_or_else(|| Error::new("review verdict absent"))?;
    require(
        ["pass", "fail", "blocked", "approve", "request_changes"].contains(&verdict),
        "invalid review verdict",
    )?;
    Ok(verdict)
}
pub fn automatic_review_workload(patch: &[u8]) -> Result<&'static str> {
    let paths = crate::files::patch_paths(patch)?;
    if paths
        .iter()
        .all(|p| p.ends_with(".md") || p.starts_with("docs/"))
    {
        Ok("routine")
    } else {
        Ok("deep")
    }
}
const STEPS: &[&str] = &[
    "settlement",
    "room_result",
    "structured_event",
    "auto_review",
    "room_quiet",
    "review_launch",
];
#[derive(Clone, Debug)]
pub struct Settlement {
    pub member: String,
    pub claim_id: String,
    pub owner: oms_reference::process::Owner,
}
impl Settlement {
    fn path(member: &str) -> String {
        format!(
            ".oms/artifacts/panel-dispatch/settlement-{}.json",
            crate::digest(member.as_bytes())
        )
    }
    pub fn claim(store: &Store, member: &str, status: i32) -> Result<Option<Self>> {
        crate::parent_only()?;
        crate::identifier(member)?;
        let owner = crate::terminals::process(std::process::id())?;
        let tx = store.transaction("panel-call-settle", Duration::from_secs(10))?;
        let path = Self::path(member);
        let old = tx.json(Path::new(&path), 65536, true)?;
        if let Some(old) = &old {
            require(
                old["schema"] == 1
                    && old["member"] == member
                    && old["exit"] == status
                    && old["steps"].is_object(),
                "invalid or conflicting settlement claim",
            )?;
            if old["complete"] == true {
                return Ok(None);
            }
            let prior = owner_from(old)?;
            if old["active"] == true && !crate::terminals::process_gone(&prior) {
                return Ok(None);
            }
        }
        let claim_id = old
            .as_ref()
            .and_then(|v| v["claim_id"].as_str())
            .map(str::to_owned)
            .unwrap_or(crate::value::unique()?);
        let mut row = old.unwrap_or_else(|| serde_json::json!({"schema":1,"member":member,"exit":status,"claim_id":claim_id,"steps":{},"complete":false}));
        row["owner_pid"] = owner.pid.into();
        row["owner_identity"] = owner.birth.clone().into();
        row["owner_uid"] = owner.uid.into();
        row["active"] = true.into();
        tx.write_json(Path::new(&path), &row, false, 65536)?;
        Ok(Some(Self {
            member: member.into(),
            claim_id,
            owner,
        }))
    }
    pub fn step(&self, store: &Store, name: &str) -> Result<()> {
        require(STEPS.contains(&name), "invalid settlement step")?;
        self.change(store, |row| {
            row["steps"][name] = true.into();
            Ok(())
        })
    }
    pub fn complete(&self, store: &Store) -> Result<()> {
        self.change(store, |row| {
            require(
                ["settlement", "room_result", "structured_event"]
                    .iter()
                    .all(|s| row["steps"][s] == true),
                "required settlement steps are incomplete",
            )?;
            row["complete"] = true.into();
            row["active"] = false.into();
            Ok(())
        })
    }
    pub fn release(&self, store: &Store) -> Result<()> {
        self.change(store, |row| {
            row["active"] = false.into();
            Ok(())
        })
    }
    fn change(&self, store: &Store, change: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
        crate::parent_only()?;
        let tx = store.transaction("panel-call-settle", Duration::from_secs(10))?;
        let path = Self::path(&self.member);
        let mut row = tx
            .json(Path::new(&path), 65536, false)?
            .ok_or_else(|| Error::new("settlement claim missing"))?;
        require(
            row["claim_id"] == self.claim_id
                && owner_from(&row)? == self.owner
                && row["active"] == true,
            "settlement claim owner changed",
        )?;
        change(&mut row)?;
        tx.write_json(Path::new(&path), &row, false, 65536)?;
        Ok(())
    }
}
fn owner_from(row: &Value) -> Result<oms_reference::process::Owner> {
    let pid = row["owner_pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| Error::new("settlement owner PID invalid"))?;
    let birth = row["owner_identity"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::new("settlement owner generation missing"))?
        .to_owned();
    let uid = row["owner_uid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| Error::new("settlement owner UID invalid"))?;
    Ok(oms_reference::process::Owner { pid, birth, uid })
}
pub fn council_metadata(
    task: &str,
    label: &str,
    rounds: usize,
    seats: &[crate::routing::Candidate],
) -> Result<Value> {
    crate::identifier(task)?;
    require(
        (1..=6).contains(&rounds) && !seats.is_empty(),
        "council requires seats and 1..6 rounds",
    )?;
    let mut providers = std::collections::BTreeSet::new();
    for seat in seats {
        crate::routing::valid_route(seat)?;
        require(
            providers.insert(&seat.provider),
            "council seats must use distinct providers",
        )?;
    }
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-council","task_id":task,"label":crate::eventlog::clean(label,120),"rounds":rounds,"seats":seats,"executes":false}),
    )
}
