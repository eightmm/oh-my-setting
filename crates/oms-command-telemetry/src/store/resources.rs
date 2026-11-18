//! Explicit observed host-local capacity in the existing dispatch transaction.
//! Parent adapters own native owner/route/contract validation; these bindings
//! are correlations, never authority reconstructed from untrusted JSON.
use super::{StoreError, before, dispatch, physical, usage};
use crate::value::{V, encode, obj, parse};
use oms_supervisor::process::{OwnedCleanup, PhysicalCall, Scope};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub domain: String,
    pub epoch: String,
    pub host_generation_sha256: String,
    pub max_teams: u64,
    pub max_invocations: u64,
    pub max_open_processes: u64,
    pub max_write_wip: u64,
    pub reviewer_headroom: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Team {
    pub id: String,
    pub spec_sha256: String,
    pub member: String,
    pub member_generation: i128,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub policy_sha256: String,
    pub host_generation_sha256: String,
    pub root_owner: String,
    pub root_generation: String,
    pub team: Option<Team>,
    pub reviewer: bool,
    pub persistent_session: bool,
    pub cleanup_scopes: Vec<Scope>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Occupancy {
    pub teams: u64,
    pub invocations: u64,
    pub open_processes: u64,
    pub write_wip: u64,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub policy: Policy,
    pub active: bool,
    pub occupancy: Occupancy,
}
fn invalid(message: &'static str) -> StoreError {
    StoreError::Invalid(message)
}
fn require(condition: bool, message: &'static str) -> Result<(), StoreError> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
fn capacity(condition: bool, resource: &'static str) -> Result<(), StoreError> {
    if condition {
        Ok(())
    } else {
        Err(StoreError::ResourceCapacity(resource))
    }
}
fn active(resource: &V) -> Result<(), StoreError> {
    if resource["state"].text() == Some("active") {
        Ok(())
    } else {
        Err(StoreError::ResourceHeld)
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn uint(value: u64) -> V {
    V::Int(value.to_string())
}
fn bytes(value: &V) -> Result<Vec<u8>, StoreError> {
    encode(value).map_err(|_| invalid("cannot encode resource registry"))
}
fn hash(value: &V) -> Result<String, StoreError> {
    Ok(format!("{:x}", Sha256::digest(bytes(value)?)))
}
fn scope(value: Scope) -> &'static str {
    match value {
        Scope::Leader => "leader",
        Scope::ProcessGroup => "process-group",
        Scope::WholeTree => "whole-tree",
    }
}
fn read_scope(value: &V) -> Result<Scope, StoreError> {
    match value.text() {
        Some("leader") => Ok(Scope::Leader),
        Some("process-group") => Ok(Scope::ProcessGroup),
        Some("whole-tree") => Ok(Scope::WholeTree),
        _ => Err(invalid("invalid resource cleanup scope")),
    }
}
impl Policy {
    pub fn parse(raw: &[u8]) -> Result<Self, StoreError> {
        oms_json::parse_with_options(
            raw,
            &oms_json::ParseOptions {
                max_bytes: 64 * 1024,
                max_depth: 16,
                ..oms_json::ParseOptions::strict()
            },
        )
        .map_err(|_| invalid("invalid resource policy JSON"))?;
        Self::decode(&decode(raw)?)
    }
    pub fn to_json(&self) -> Result<Vec<u8>, StoreError> {
        bytes(&self.value()?)
    }
    pub(super) fn value(&self) -> Result<V, StoreError> {
        require(
            id(&self.domain) && id(&self.epoch) && digest(&self.host_generation_sha256),
            "invalid resource domain identity",
        )?;
        require(
            [
                self.max_teams,
                self.max_invocations,
                self.max_open_processes,
                self.max_write_wip,
            ]
            .iter()
            .all(|n| *n > 0)
                && self.reviewer_headroom < self.max_invocations,
            "invalid finite resource limits",
        )?;
        Ok(
            obj!("schema":1,"kind":"oms-resource-policy","scope":"repository-host-local-observed",
            "domain":self.domain.clone(),"epoch":self.epoch.clone(),"host_generation_sha256":self.host_generation_sha256.clone(),
            "max_teams":uint(self.max_teams),"max_invocations":uint(self.max_invocations),
            "max_open_processes":uint(self.max_open_processes),"max_write_wip":uint(self.max_write_wip),
            "reviewer_headroom":uint(self.reviewer_headroom)),
        )
    }
    pub fn sha256(&self) -> Result<String, StoreError> {
        hash(&self.value()?)
    }
    pub(super) fn decode(value: &V) -> Result<Self, StoreError> {
        usage::keys(
            value,
            &[
                "schema",
                "kind",
                "scope",
                "domain",
                "epoch",
                "host_generation_sha256",
                "max_teams",
                "max_invocations",
                "max_open_processes",
                "max_write_wip",
                "reviewer_headroom",
            ],
        )?;
        require(
            value["schema"] == V::from(1)
                && value["kind"].text() == Some("oms-resource-policy")
                && value["scope"].text() == Some("repository-host-local-observed"),
            "unsupported resource policy scope",
        )?;
        let policy = Self {
            domain: value["domain"].text().unwrap_or_default().into(),
            epoch: value["epoch"].text().unwrap_or_default().into(),
            host_generation_sha256: value["host_generation_sha256"]
                .text()
                .unwrap_or_default()
                .into(),
            max_teams: usage::uint(&value["max_teams"])?,
            max_invocations: usage::uint(&value["max_invocations"])?,
            max_open_processes: usage::uint(&value["max_open_processes"])?,
            max_write_wip: usage::uint(&value["max_write_wip"])?,
            reviewer_headroom: usage::uint(&value["reviewer_headroom"])?,
        };
        policy.value()?;
        Ok(policy)
    }
}
impl Binding {
    pub(super) fn value(&self) -> Result<V, StoreError> {
        let rank = |scope| match scope {
            Scope::Leader => 0,
            Scope::ProcessGroup => 1,
            Scope::WholeTree => 2,
        };
        require(
            digest(&self.policy_sha256)
                && digest(&self.host_generation_sha256)
                && id(&self.root_owner)
                && id(&self.root_generation)
                && !self.cleanup_scopes.is_empty()
                && self.cleanup_scopes.len() <= 3
                && self
                    .cleanup_scopes
                    .windows(2)
                    .all(|pair| rank(pair[0]) < rank(pair[1])),
            "invalid resource owner binding",
        )?;
        let team = if let Some(team) = &self.team {
            require(
                id(&team.id)
                    && id(&team.member)
                    && digest(&team.spec_sha256)
                    && team.member_generation > 0,
                "invalid resource team binding",
            )?;
            obj!("id":team.id.clone(),"spec_sha256":team.spec_sha256.clone(),"member":team.member.clone(),
                "member_generation":V::Int(team.member_generation.to_string()))
        } else {
            V::Null
        };
        Ok(
            obj!("policy_sha256":self.policy_sha256.clone(),"host_generation_sha256":self.host_generation_sha256.clone(),
            "root_owner":self.root_owner.clone(),"root_generation":self.root_generation.clone(),"team":team,
            "reviewer":self.reviewer,"persistent_session":self.persistent_session,
            "cleanup_scopes":V::Array(self.cleanup_scopes.iter().map(|s| V::from(scope(*s))).collect())),
        )
    }
    pub(super) fn decode(value: &V) -> Result<Self, StoreError> {
        usage::keys(
            value,
            &[
                "policy_sha256",
                "host_generation_sha256",
                "root_owner",
                "root_generation",
                "team",
                "reviewer",
                "persistent_session",
                "cleanup_scopes",
            ],
        )?;
        let team = if value["team"] == V::Null {
            None
        } else {
            let team = &value["team"];
            usage::keys(team, &["id", "spec_sha256", "member", "member_generation"])?;
            Some(Team {
                id: team["id"].text().unwrap_or_default().into(),
                spec_sha256: team["spec_sha256"].text().unwrap_or_default().into(),
                member: team["member"].text().unwrap_or_default().into(),
                member_generation: team["member_generation"]
                    .int()
                    .and_then(|n| n.parse().ok())
                    .ok_or(invalid("invalid member generation"))?,
            })
        };
        let (V::Bool(reviewer), V::Bool(persistent_session)) =
            (&value["reviewer"], &value["persistent_session"])
        else {
            return Err(invalid("invalid resource lifetime"));
        };
        let binding = Self {
            policy_sha256: value["policy_sha256"].text().unwrap_or_default().into(),
            host_generation_sha256: value["host_generation_sha256"]
                .text()
                .unwrap_or_default()
                .into(),
            root_owner: value["root_owner"].text().unwrap_or_default().into(),
            root_generation: value["root_generation"].text().unwrap_or_default().into(),
            team,
            reviewer: *reviewer,
            persistent_session: *persistent_session,
            cleanup_scopes: match &value["cleanup_scopes"] {
                V::Array(scopes) => scopes.iter().map(read_scope).collect::<Result<_, _>>()?,
                _ => return Err(invalid("invalid cleanup scope set")),
            },
        };
        binding.value()?;
        Ok(binding)
    }
}
fn read(registry: &V) -> Result<Option<(&V, Policy)>, StoreError> {
    if registry["schema"] == V::from(1) {
        require(
            registry.get("resources").is_none(),
            "resource domain lacks its version barrier",
        )?;
        if let V::Object(calls) = &registry["physical"]["calls"] {
            require(
                !calls.values().any(|record| {
                    record["schema"] == V::from(3)
                        || record["request"].get("resource_binding").is_some()
                        || record["request"]["binding"].text() == Some("resource-bound")
                }),
                "resource physical call lost its domain",
            )?;
        }
        return Ok(None);
    }
    require(
        registry["schema"] == V::from(2),
        "unsupported resource registry version",
    )?;
    require(
        matches!(&registry["calls"], V::Object(calls) if calls.is_empty()),
        "legacy dispatch custody lacks resource claims",
    )?;
    let resource = &registry["resources"];
    usage::keys(
        resource,
        &[
            "schema",
            "policy",
            "policy_sha256",
            "state",
            "teams",
            "claims",
            "write_wip",
        ],
    )?;
    require(
        resource["schema"] == V::from(1)
            && matches!(resource["state"].text(), Some("active" | "held"))
            && matches!(resource["teams"], V::Object(_))
            && matches!(resource["claims"], V::Object(_))
            && matches!(resource["write_wip"], V::Object(_)),
        "invalid resource domain",
    )?;
    let policy = Policy::decode(&resource["policy"])?;
    require(
        resource["policy_sha256"].text() == Some(policy.sha256()?.as_str()),
        "resource policy digest changed",
    )?;
    Ok(Some((resource, policy)))
}
fn checked_add(value: &mut u64, n: u64) -> Result<(), StoreError> {
    *value = value
        .checked_add(n)
        .ok_or(invalid("resource occupancy overflow"))?;
    Ok(())
}
fn count(registry: &V, resource: &V, policy: &Policy) -> Result<Occupancy, StoreError> {
    let mut total = Occupancy::default();
    let policy_sha256 = policy.sha256()?;
    let V::Object(teams) = &resource["teams"] else {
        return Err(invalid("invalid resource teams"));
    };
    for (id, team) in teams {
        usage::keys(
            team,
            &[
                "id",
                "spec_sha256",
                "root_owner",
                "root_generation",
                "active",
            ],
        )?;
        require(
            id_text(&team["id"])
                && team["spec_sha256"]
                    .text()
                    .is_some_and(|s| id == s.as_bytes() && digest(s))
                && id_text(&team["root_owner"])
                && id_text(&team["root_generation"]),
            "invalid resource team slot",
        )?;
        let V::Bool(active) = team["active"] else {
            return Err(invalid("invalid team custody"));
        };
        checked_add(&mut total.teams, u64::from(active))?;
    }
    let V::Object(claims) = &resource["claims"] else {
        return Err(invalid("invalid resource claims"));
    };
    for (id, claim) in claims {
        usage::keys(
            claim,
            &[
                "call_id",
                "operation_id",
                "request_sha256",
                "binding",
                "invocation",
                "process",
                "revision",
            ],
        )?;
        require(
            claim["call_id"]
                .text()
                .is_some_and(|s| id == s.as_bytes() && crate::store::resources::id(s))
                && id_text(&claim["operation_id"])
                && digest(claim["request_sha256"].text().unwrap_or_default()),
            "invalid resource claim",
        )?;
        let binding = Binding::decode(&claim["binding"])?;
        require(
            binding.policy_sha256 == policy_sha256
                && binding.host_generation_sha256 == policy.host_generation_sha256,
            "resource claim belongs to another domain",
        )?;
        let physical = &registry["physical"]["calls"][claim["call_id"].text().unwrap()];
        require(
            physical != &V::Null
                && hash(&physical["request"])? == claim["request_sha256"].text().unwrap()
                && physical["request"]["identity"]["operation_id"] == claim["operation_id"]
                && physical["schema"] == V::from(3)
                && physical["request"]["resource_binding"] == claim["binding"],
            "resource claim lost physical intent",
        )?;
        let (V::Bool(invocation), V::Bool(process)) = (&claim["invocation"], &claim["process"])
        else {
            return Err(invalid("invalid execution custody"));
        };
        require(
            usage::uint(&claim["revision"])? > 0 && (!invocation || *process),
            "invalid execution custody revision",
        )?;
        checked_add(&mut total.invocations, u64::from(*invocation))?;
        checked_add(&mut total.open_processes, u64::from(*process))?;
        if physical["request"]["access"].text() == Some("write") {
            let wip = &resource["write_wip"][claim["operation_id"].text().unwrap()];
            require(
                wip["owner"] == write_owner(&binding),
                "physical writer lost write custody",
            )?;
        }
        if let Some(team) = &binding.team {
            let slot = &resource["teams"][&team.spec_sha256];
            require(
                slot["id"].text() == Some(&team.id)
                    && slot["root_owner"].text() == Some(&binding.root_owner)
                    && slot["root_generation"].text() == Some(&binding.root_generation),
                "physical call lost its team slot",
            )?;
            require(
                slot["active"] == V::Bool(true)
                    || (!invocation
                        && !process
                        && (physical["request"]["access"].text() != Some("write")
                            || resource["write_wip"][claim["operation_id"].text().unwrap()]["active"]
                                == V::Bool(false))),
                "active custody lost its team slot",
            )?;
        }
    }
    let V::Object(wip) = &resource["write_wip"] else {
        return Err(invalid("invalid write custody"));
    };
    for (operation, entry) in wip {
        usage::keys(entry, &["operation_id", "owner", "active"])?;
        require(
            entry["operation_id"]
                .text()
                .is_some_and(|s| operation == s.as_bytes() && crate::store::resources::id(s)),
            "invalid write operation",
        )?;
        usage::keys(&entry["owner"], &["root_owner", "root_generation", "team"])?;
        require(
            id_text(&entry["owner"]["root_owner"])
                && id_text(&entry["owner"]["root_generation"])
                && claims.values().any(|claim| {
                    claim["operation_id"] == entry["operation_id"]
                        && registry["physical"]["calls"]
                            [claim["call_id"].text().unwrap_or_default()]["request"]["access"]
                            .text()
                            == Some("write")
                }),
            "write custody has no physical writer",
        )?;
        let V::Bool(active) = entry["active"] else {
            return Err(invalid("invalid write custody"));
        };
        checked_add(&mut total.write_wip, u64::from(active))?;
    }
    require(
        total.teams <= policy.max_teams
            && total.invocations <= policy.max_invocations
            && total.open_processes <= policy.max_open_processes
            && total.write_wip <= policy.max_write_wip,
        "resource occupancy exceeds policy",
    )?;
    Ok(total)
}
fn id_text(value: &V) -> bool {
    value.text().is_some_and(id)
}
pub(super) fn validate(registry: &V) -> Result<(), StoreError> {
    if let Some((resource, policy)) = read(registry)? {
        count(registry, resource, &policy)?;
        if registry.get("physical").is_some() {
            for intent in physical::intents(&registry["physical"])? {
                require(
                    resource["claims"][&intent.identity.call_id] != V::Null
                        && intent.resource_binding.is_some(),
                    "physical call bypassed resource claims",
                )?;
            }
        }
    }
    Ok(())
}
pub(super) fn validate_receipts(
    registry: &V,
    intents: &[physical::Intent],
    receipts: &usage::Projection,
) -> Result<(), StoreError> {
    if registry["schema"] == V::from(2) {
        let known = intents
            .iter()
            .map(|intent| (intent.identity.call_id.as_str(), &intent.identity))
            .collect::<BTreeMap<_, _>>();
        for call in &receipts.calls {
            require(
                known
                    .get(call.receipt.identity.call_id.as_str())
                    .is_some_and(|identity| **identity == call.receipt.identity),
                "resource domain has unclaimed usage custody",
            )?;
        }
    }
    Ok(())
}
pub(super) fn has_contract(value: &oms_json::Value) -> bool {
    let physical = value
        .get("physical")
        .and_then(|value| value.get("calls"))
        .and_then(oms_json::Value::as_object);
    value.get("resources").is_some()
        || physical.is_some_and(|calls| {
            calls.values().any(|record| {
                record
                    .get("schema")
                    .and_then(oms_json::Value::as_integer)
                    .is_some_and(|n| n.as_decimal() == "3")
                    || record.get("request").is_some_and(|request| {
                        request.get("resource_binding").is_some()
                            || request
                                .get("binding")
                                .and_then(oms_json::Value::as_string)
                                .is_some_and(|s| s == &oms_json::PyString::from("resource-bound"))
                    })
            })
        })
}
fn decode(raw: &[u8]) -> Result<V, StoreError> {
    parse(raw, true).map_err(|_| invalid("invalid resource JSON"))
}
fn transaction<'a>(
    repo: &Path,
    stop: &'a AtomicBool,
    deadline: Instant,
) -> Result<dispatch::Transaction<'a>, dispatch::Error> {
    before(stop, Some(deadline)).map_err(|source| dispatch::Error {
        source,
        publication_uncertain: false,
    })?;
    dispatch::Transaction::acquire(
        repo,
        deadline.saturating_duration_since(Instant::now()),
        stop,
    )
}

/// Explicit parent activation. Legacy history lacks scoped custody attestations;
/// it is preserved and refused rather than silently reset to an empty epoch.
pub fn activate(
    repo: &Path,
    policy: &Policy,
    expected: Option<&[u8]>,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), dispatch::Error> {
    let tx = transaction(repo, stop, deadline)?;
    let raw = tx.snapshot()?;
    if raw.as_deref() != expected {
        return Err(tx.error(StoreError::Changed));
    }
    let mut registry = raw
        .as_deref()
        .map(decode)
        .transpose()
        .map_err(|e| tx.error(e))?
        .unwrap_or_else(|| obj!("schema":1,"calls":obj!()));
    let value = policy.value().map_err(|e| tx.error(e))?;
    let digest = policy.sha256().map_err(|e| tx.error(e))?;
    if let Some((resource, _)) = read(&registry).map_err(|e| tx.error(e))? {
        require(
            resource["policy_sha256"].text() == Some(&digest),
            "resource epoch already exists",
        )
        .map_err(|e| tx.error(e))?;
        active(resource).map_err(|e| tx.error(e))?;
        return Ok(());
    }
    let _usage_writer = usage::writer_before(tx.repo(), stop, deadline).map_err(|e| tx.error(e))?;
    let physical = physical::load(tx.repo(), stop).map_err(|e| tx.error(e))?;
    let usage = usage::load(tx.repo()).map_err(|e| tx.error(e))?;
    require(
        matches!(&registry["calls"], V::Object(calls) if calls.is_empty())
            && physical.is_empty()
            && usage.calls.is_empty(),
        "legacy custody must be reconciled before resource activation",
    )
    .map_err(|e| tx.error(e))?;
    registry["schema"] = V::from(2);
    registry["resources"] = obj!("schema":1,"policy":value,"policy_sha256":digest,"state":"active",
        "teams":obj!(),"claims":obj!(),"write_wip":obj!());
    before(stop, Some(deadline)).map_err(|e| tx.error(e))?;
    tx.replace(raw.as_deref(), &bytes(&registry).map_err(|e| tx.error(e))?)
}

pub fn occupancy(repo: &Path, stop: &AtomicBool) -> Result<Option<Occupancy>, StoreError> {
    Ok(snapshot(repo, stop)?.map(|snapshot| snapshot.occupancy))
}
/// Read-only observation. Admission always rechecks it under the dispatch writer.
pub fn snapshot(repo: &Path, stop: &AtomicBool) -> Result<Option<Snapshot>, StoreError> {
    let Some(raw) = dispatch::read_snapshot(repo, stop)? else {
        return Ok(None);
    };
    let registry = decode(&raw)?;
    read(&registry)?
        .map(|(resource, policy)| {
            Ok(Snapshot {
                occupancy: count(&registry, resource, &policy)?,
                active: resource["state"].text() == Some("active"),
                policy,
            })
        })
        .transpose()
}

/// The native parent holds its current contract fence. A team slot is separate
/// from its members' invocation/process claims and is never inferred from them.
pub fn reserve_team(
    repo: &Path,
    binding: &Binding,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), dispatch::Error> {
    let tx = transaction(repo, stop, deadline)?;
    let raw = tx
        .snapshot()?
        .ok_or_else(|| tx.error(invalid("resource domain missing")))?;
    let mut registry = decode(&raw).map_err(|e| tx.error(e))?;
    binding.value().map_err(|e| tx.error(e))?;
    let team = binding
        .team
        .as_ref()
        .ok_or_else(|| tx.error(invalid("team slot requires a team")))?;
    let (resource, policy) = read(&registry)
        .map_err(|e| tx.error(e))?
        .ok_or_else(|| tx.error(invalid("resource domain missing")))?;
    require(
        policy.sha256().map_err(|e| tx.error(e))? == binding.policy_sha256
            && policy.host_generation_sha256 == binding.host_generation_sha256,
        "resource domain is held or changed",
    )
    .map_err(|e| tx.error(e))?;
    active(resource).map_err(|e| tx.error(e))?;
    let slot = obj!("id":team.id.clone(),"spec_sha256":team.spec_sha256.clone(),"root_owner":binding.root_owner.clone(),
        "root_generation":binding.root_generation.clone(),"active":true);
    if resource["teams"][&team.spec_sha256] != V::Null {
        require(
            resource["teams"][&team.spec_sha256] == slot,
            "team resource slot conflicts",
        )
        .map_err(|e| tx.error(e))?;
        return Ok(());
    }
    let total = count(&registry, resource, &policy).map_err(|e| tx.error(e))?;
    capacity(total.teams < policy.max_teams, "teams").map_err(|e| tx.error(e))?;
    registry["resources"]["teams"][&team.spec_sha256] = slot;
    before(stop, Some(deadline)).map_err(|e| tx.error(e))?;
    tx.replace(Some(&raw), &bytes(&registry).map_err(|e| tx.error(e))?)
}

/// Blocks new admission while retaining all claims and permitting settlement.
pub fn hold(
    repo: &Path,
    expected_policy_sha256: &str,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), dispatch::Error> {
    let tx = transaction(repo, stop, deadline)?;
    let raw = tx
        .snapshot()?
        .ok_or_else(|| tx.error(invalid("resource domain missing")))?;
    let mut registry = decode(&raw).map_err(|e| tx.error(e))?;
    let (resource, _) = read(&registry)
        .map_err(|e| tx.error(e))?
        .ok_or_else(|| tx.error(invalid("resource domain missing")))?;
    require(
        resource["policy_sha256"].text() == Some(expected_policy_sha256),
        "resource policy changed",
    )
    .map_err(|e| tx.error(e))?;
    if resource["state"].text() == Some("held") {
        return Ok(());
    }
    registry["resources"]["state"] = "held".into();
    before(stop, Some(deadline)).map_err(|e| tx.error(e))?;
    tx.replace(Some(&raw), &bytes(&registry).map_err(|e| tx.error(e))?)
}

/// Called only while the existing physical producer owns its dispatch writer.
pub(super) fn reserve(
    registry: &mut V,
    identity: &usage::Identity,
    write: bool,
    binding: Option<&Binding>,
) -> Result<(), StoreError> {
    let Some((resource, policy)) = read(registry)? else {
        return require(
            binding.is_none(),
            "bound resource call requires an active domain",
        );
    };
    let binding = binding.ok_or(invalid("resource domain requires native bound admission"))?;
    let binding_value = binding.value()?;
    require(
        binding.policy_sha256 == policy.sha256()?
            && binding.host_generation_sha256 == policy.host_generation_sha256,
        "resource domain is held or changed",
    )?;
    active(resource)?;
    let mut total = count(registry, resource, &policy)?;
    checked_add(&mut total.invocations, 1)?;
    checked_add(&mut total.open_processes, 1)?;
    capacity(
        total.invocations
            <= if binding.reviewer {
                policy.max_invocations
            } else {
                policy.max_invocations - policy.reviewer_headroom
            },
        "invocations",
    )?;
    capacity(
        total.open_processes <= policy.max_open_processes,
        "open processes",
    )?;
    if let Some(team) = &binding.team {
        let saved = &resource["teams"][&team.spec_sha256];
        require(
            saved["active"] == V::Bool(true)
                && saved["spec_sha256"].text() == Some(&team.spec_sha256)
                && saved["root_owner"].text() == Some(&binding.root_owner)
                && saved["root_generation"].text() == Some(&binding.root_generation),
            "team has no current resource slot",
        )?;
    }
    let owner = write_owner(binding);
    let existing_wip = &resource["write_wip"][&identity.operation_id];
    if write {
        if existing_wip == &V::Null {
            checked_add(&mut total.write_wip, 1)?;
        } else {
            require(
                existing_wip["owner"] == owner && existing_wip["active"] == V::Bool(true),
                "write custody owner changed",
            )?;
        }
        let V::Object(claims) = &resource["claims"] else {
            unreachable!()
        };
        require(
            !claims.values().any(|claim| {
                claim["operation_id"].text() == Some(&identity.operation_id)
                    && claim["invocation"] == V::Bool(true)
            }),
            "another physical writer still owns this operation",
        )?;
        capacity(total.write_wip <= policy.max_write_wip, "write WIP")?;
    }
    let request = &registry["physical"]["calls"][&identity.call_id]["request"];
    let request_hash = hash(request)?;
    let claim = obj!("call_id":identity.call_id.clone(),"operation_id":identity.operation_id.clone(),
        "request_sha256":request_hash,"binding":binding_value,"invocation":true,"process":true,"revision":1);
    let V::Object(claims) = &mut registry["resources"]["claims"] else {
        unreachable!()
    };
    require(
        !claims.contains_key(identity.call_id.as_bytes()),
        "resource claim already exists",
    )?;
    claims.insert(identity.call_id.as_bytes().to_vec(), claim);
    if write {
        registry["resources"]["write_wip"][&identity.operation_id] =
            obj!("operation_id":identity.operation_id.clone(),"owner":owner,"active":true);
    }
    Ok(())
}
fn write_owner(binding: &Binding) -> V {
    obj!("root_owner":binding.root_owner.clone(),"root_generation":binding.root_generation.clone(),
        "team":binding.team.as_ref().map(|team| obj!("id":team.id.clone(),"spec_sha256":team.spec_sha256.clone(),
            "member":team.member.clone(),"member_generation":V::Int(team.member_generation.to_string()))))
}

/// Rechecks the owned reservation after outer contract validation. The caller
/// still needs its original New acknowledgement; this observation grants no resend.
pub fn fence(
    repo: &Path,
    binding: &Binding,
    call: &PhysicalCall,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), dispatch::Error> {
    let tx = transaction(repo, stop, deadline)?;
    let raw = tx
        .snapshot()?
        .ok_or_else(|| tx.error(invalid("resource registry missing")))?;
    let registry = decode(&raw).map_err(|e| tx.error(e))?;
    let (resource, policy) = read(&registry)
        .map_err(|e| tx.error(e))?
        .ok_or_else(|| tx.error(invalid("resource domain missing")))?;
    active(resource).map_err(|e| tx.error(e))?;
    let claim = &resource["claims"][call.id()];
    require(
        binding.policy_sha256 == policy.sha256().map_err(|e| tx.error(e))?
            && claim["binding"] == binding.value().map_err(|e| tx.error(e))?
            && claim["invocation"] == V::Bool(true)
            && claim["process"] == V::Bool(true)
            && registry["physical"]["calls"][call.id()]["state"].text() == Some("ready"),
        "physical reservation is not ready or changed",
    )
    .map_err(|e| tx.error(e))?;
    let intents = physical::intents(&registry["physical"]).map_err(|e| tx.error(e))?;
    let receipts = usage::load(tx.repo()).map_err(|e| tx.error(e))?;
    validate_receipts(&registry, &intents, &receipts).map_err(|e| tx.error(e))?;
    require(
        intents
            .iter()
            .find(|intent| intent.identity.call_id == call.id())
            .is_some_and(|intent| {
                receipts.calls.iter().any(|entry| {
                    entry.receipt.identity == intent.identity
                        && Some(entry.receipt.ordinal) == intent.usage_ordinal
                        && entry.receipt.cumulative == intent.cumulative
                })
            }),
        "physical reservation has no matching usage acknowledgement",
    )
    .map_err(|e| tx.error(e))?;
    before(stop, Some(deadline)).map_err(|e| tx.error(e))
}

/// Settles only the exact observed execution scope. Persistent sessions and
/// write candidates keep their independent obligations; usage is untouched.
pub fn settle_execution(
    repo: &Path,
    binding: &Binding,
    expected_call: &PhysicalCall,
    proof: &OwnedCleanup,
    expected_revision: u64,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), dispatch::Error> {
    let tx = transaction(repo, stop, deadline)?;
    let raw = tx
        .snapshot()?
        .ok_or_else(|| tx.error(invalid("resource registry missing")))?;
    let mut registry = decode(&raw).map_err(|e| tx.error(e))?;
    validate(&registry).map_err(|e| tx.error(e))?;
    let call = expected_call.id();
    let claim = &registry["resources"]["claims"][call];
    let expected = binding.value().map_err(|e| tx.error(e))?;
    require(
        claim["binding"] == expected
            && binding
                .cleanup_scopes
                .iter()
                .any(|scope| proof.matches(expected_call, *scope)),
        "resource settlement identity, scope or revision changed",
    )
    .map_err(|e| tx.error(e))?;
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| tx.error(invalid("resource revision overflow")))?;
    let current_revision = usage::uint(&claim["revision"]).map_err(|e| tx.error(e))?;
    if current_revision == revision
        && claim["invocation"] == V::Bool(false)
        && claim["process"] == V::Bool(binding.persistent_session)
    {
        return Ok(());
    }
    require(
        current_revision == expected_revision && claim["invocation"] == V::Bool(true),
        "resource settlement revision changed",
    )
    .map_err(|e| tx.error(e))?;
    let claim = &mut registry["resources"]["claims"][call];
    claim["invocation"] = V::Bool(false);
    if !binding.persistent_session {
        claim["process"] = V::Bool(false);
    }
    claim["revision"] = uint(revision);
    before(stop, Some(deadline)).map_err(|e| tx.error(e))?;
    tx.replace(Some(&raw), &bytes(&registry).map_err(|e| tx.error(e))?)
}
