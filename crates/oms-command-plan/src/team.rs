//! Immutable team contracts; the existing plan is the sole activation writer.
//! Registration grants no launch, patch acceptance or publication authority.
use crate::engine::{clean_paths, parse_ts, plain, receipt_id, screen, valid_id};
use crate::json::{self, Value, empty, object, string};
use crate::storage::{MAX_STREAM, rows};
use crate::{Action, Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::time::{Duration, Instant};

pub const MAX_SPEC_BYTES: usize = 64 * 1024;
const MAX_MEMBERS: usize = 128;
const MAX_TEAMS: usize = 256;

#[derive(Clone, Debug)]
pub struct TeamSpec {
    pub value: Value,
}
#[derive(Clone, Debug)]
pub struct MemberBinding {
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct AdmissionRequest {
    pub team_id: String,
    pub spec_sha256: String,
    pub member_id: String,
    pub member_generation: i128,
    pub provider: String,
    pub harness: String,
    pub host: String,
    pub transport: String,
    pub account: Option<String>,
    pub selected_model: Option<String>,
    pub write: bool,
}

/// Pins canonical contract eligibility, not resource capacity, actual member
/// enrollment or a launch permit. Callers reserve through the existing physical
/// producer, fence again, and drop all producer guards before external execution.
pub struct AdmissionGuard<'p, 's> {
    plan: &'p Plan<'s>,
    request: AdmissionRequest,
    deadline: Instant,
    binding: Value,
    lifecycle: Box<dyn crate::services::LifecycleGuard + 'p>,
    locks: Vec<oms_lock::Guard>,
}
fn admission_before(plan: &Plan<'_>, deadline: Instant) -> Result<()> {
    if plan.interrupted.load(std::sync::atomic::Ordering::Acquire) {
        return Err(crate::Error {
            code: 130,
            message: "team admission interrupted".into(),
        });
    }
    if Instant::now() >= deadline {
        return Err(crate::Error {
            code: 75,
            message: "team admission deadline expired".into(),
        });
    }
    Ok(())
}
fn admission_time() -> Result<String> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| error("invalid team admission time"))?;
    Ok(
        chrono::DateTime::<chrono::Utc>::from_timestamp(time.as_secs() as i64, 0)
            .ok_or_else(|| error("invalid team admission time"))?
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}
impl<'s> Plan<'s> {
    pub fn team_admission_before<'p>(
        &'p self,
        request: AdmissionRequest,
        deadline: Instant,
    ) -> Result<AdmissionGuard<'p, 's>> {
        self.team_admission_inner(request, deadline)
            .map_err(|error| admission_before(self, deadline).err().unwrap_or(error))
    }
    fn team_admission_inner<'p>(
        &'p self,
        request: AdmissionRequest,
        deadline: Instant,
    ) -> Result<AdmissionGuard<'p, 's>> {
        self.context.parent()?;
        require(
            self.context.canonical(),
            "team admission requires the canonical plan",
        )?;
        require(
            crate::engine::room_id(&self.context.room_id),
            "team admission requires an enrolled room",
        )?;
        admission_before(self, deadline)?;
        let mut locks = Vec::new();
        // Same writer order as Plan::locked; physical/campaign locks come later.
        for relative in [
            ".oms/landings.jsonl".to_owned(),
            format!(".oms/threads/{}.jsonl", self.context.room_id),
            ".oms/delegations/.marker-set-lock-target".into(),
            crate::storage::ACTIVE.into(),
        ] {
            let mut settings = self.context.lock_settings.clone();
            settings.timeout = settings
                .timeout
                .min(deadline.saturating_duration_since(Instant::now()));
            admission_before(self, deadline)?;
            let lock_deadline = Instant::now() + settings.timeout;
            let authority =
                oms_lock::Lock::for_file(&self.context.state_repo.join(relative), settings)
                    .map_err(|e| crate::Error {
                        code: e.exit_code(),
                        message: e.to_string(),
                    })?;
            let guard = loop {
                admission_before(self, deadline)?;
                if Instant::now() >= lock_deadline {
                    return Err(crate::Error {
                        code: 75,
                        message: "team admission writer is busy".into(),
                    });
                }
                match authority.try_acquire() {
                    Ok(guard) => break guard,
                    Err(oms_lock::Error::Busy) => std::thread::sleep(
                        Duration::from_millis(20)
                            .min(lock_deadline.saturating_duration_since(Instant::now())),
                    ),
                    Err(error) => {
                        return Err(crate::Error {
                            code: error.exit_code(),
                            message: error.to_string(),
                        });
                    }
                }
            };
            locks.push(guard);
        }
        let lifecycle = self
            .services
            .lifecycle_writer_before(&self.context.state_repo, deadline)
            .map_err(|error| admission_before(self, deadline).err().unwrap_or(error))?;
        let mut guard = AdmissionGuard {
            plan: self,
            request,
            deadline,
            binding: Value::Null,
            lifecycle,
            locks,
        };
        guard.binding = guard.current()?;
        guard.check()?;
        Ok(guard)
    }
}
impl AdmissionGuard<'_, '_> {
    pub fn binding(&self) -> &Value {
        &self.binding
    }
    pub fn check(&self) -> Result<()> {
        self.check_current().map_err(|error| {
            admission_before(self.plan, self.deadline)
                .err()
                .unwrap_or(error)
        })
    }
    fn check_current(&self) -> Result<()> {
        admission_before(self.plan, self.deadline)?;
        for lock in &self.locks {
            lock.check().map_err(|e| error(e.to_string()))?;
        }
        self.lifecycle.check()?;
        require(
            self.current()? == self.binding,
            "team admission binding changed",
        )?;
        self.lifecycle.check()?;
        for lock in &self.locks {
            lock.check().map_err(|e| error(e.to_string()))?;
        }
        require(
            parse_ts(self.binding.text("expires_at")) > parse_ts(&admission_time()?),
            "team contract expired",
        )?;
        admission_before(self.plan, self.deadline)
    }
    fn current(&self) -> Result<Value> {
        admission_before(self.plan, self.deadline)?;
        let mut context = self.plan.context.clone();
        context.ts = admission_time()?;
        let mut plan = Plan::open(context, self.plan.services, self.plan.interrupted)?;
        plan.proof_deadline = Some(self.deadline);
        let groups = std::sync::Arc::new(crate::storage::GroupProofSnapshot::new());
        plan.group_snapshot = Some(groups.clone());
        plan.store = plan.store.with_group_snapshot(Some(groups.clone()));
        plan.source = plan.source.with_group_snapshot(Some(groups.clone()));
        loop {
            admission_before(self.plan, self.deadline)?;
            let result = self.current_binding(&plan);
            if let Some(gid) = groups.missing()? {
                let proof = plan.services.group_membership_before(
                    gid,
                    self.deadline,
                    self.plan.interrupted,
                )?;
                groups.insert(gid, proof)?;
                continue;
            }
            let binding = result?;
            for proof in groups.proofs()? {
                let current = plan.services.group_membership_before(
                    proof.gid,
                    self.deadline,
                    self.plan.interrupted,
                )?;
                require(
                    current == proof,
                    "native group ownership changed during team admission",
                )?;
            }
            require(
                plan.git().text(&["rev-parse", "HEAD"])? == binding.text("base_sha"),
                "team base changed",
            )?;
            require(
                json::sha(
                    plan.source
                        .read(binding.text("policy_ref"), MAX_SPEC_BYTES)?,
                ) == binding.text("policy_sha256"),
                "team policy changed",
            )?;
            admission_before(self.plan, self.deadline)?;
            return Ok(binding);
        }
    }
    fn current_binding(&self, plan: &Plan<'_>) -> Result<Value> {
        let snapshot = plan.load()?;
        let (teams, heads, _) = registry(plan, &snapshot)?;
        let request = &self.request;
        let record = teams
            .opt(&request.team_id)
            .ok_or_else(|| error("no such team"))?;
        let spec = &heads[&request.team_id];
        require(
            record.text("phase") != "settled",
            "team admission was withdrawn",
        )?;
        require(
            record.text("phase") == "planned"
                || record.text("phase") == "held"
                    && record.text("reason") == "new generation requires admission",
            "team admission is held",
        )?;
        require(
            record
                .get("versions")
                .array()?
                .last()
                .unwrap()
                .text("sha256")
                == request.spec_sha256,
            "team admission head changed",
        )?;
        require(
            plan.context.panel_main_attempt == spec.text("root_owner")
                && plan.context.room_id == spec.text("room_id"),
            "team admission belongs to another main",
        )?;
        current(&mut Bindings::new(plan), &snapshot, spec)?;
        let task = snapshot.task(spec.text("task_id"))?;
        require(
            snapshot.deps_done(task)
                && ["ready", "claimed", "running", "review"].contains(&task.text("state")),
            "team task dependencies or state are not ready",
        )?;
        let member = spec
            .get("members")
            .array()?
            .iter()
            .find(|member| member.text("id") == request.member_id)
            .ok_or_else(|| error("no such team member"))?;
        require(
            member.get("generation").is_int(request.member_generation),
            "team member generation changed",
        )?;
        let route = member.get("route");
        let expected_model = ["resolved_model", "requested_model", "configured_model"]
            .iter()
            .filter_map(|key| route.opt(key))
            .find(|value| **value != Value::Null)
            .map(Value::str)
            .transpose()?;
        require(
            plan.services.normalize_provider(&request.provider)? == request.provider
                && route.text("provider") == request.provider
                && route.text("harness") == request.harness
                && route.text("host") == request.host
                && route.text("transport") == request.transport
                && route
                    .opt("account")
                    .filter(|v| **v != Value::Null)
                    .map(Value::str)
                    .transpose()?
                    == request.account.as_deref()
                && expected_model == request.selected_model.as_deref()
                && member.text("access") == if request.write { "write" } else { "read" },
            "physical route or access differs from team contract",
        )?;
        admission_before(self.plan, self.deadline)?;
        Ok(object([
            ("schema", crate::json::number(1)),
            ("team_id", string(&request.team_id)),
            ("team_spec_sha256", string(&request.spec_sha256)),
            ("team_generation", spec.get("generation").clone()),
            ("member", member.clone()),
            ("root_owner", spec.get("root_owner").clone()),
            (
                "root_owner_generation",
                spec.get("root_owner_generation").clone(),
            ),
            ("room_id", spec.get("room_id").clone()),
            ("plan_id", spec.get("plan_id").clone()),
            ("task_id", spec.get("task_id").clone()),
            ("task_sha256", spec.get("task_sha256").clone()),
            ("criterion_sha256", spec.get("criterion_sha256").clone()),
            ("base_sha", spec.get("base_sha").clone()),
            ("policy_ref", spec.get("policy_ref").clone()),
            ("policy_sha256", spec.get("policy_sha256").clone()),
            ("budget_ref", spec.get("budget_ref").clone()),
            ("expires_at", spec.get("expires_at").clone()),
            ("allowed_paths", spec.get("allowed_paths").clone()),
            ("forbidden_paths", spec.get("forbidden_paths").clone()),
            ("resources", spec.get("resources").clone()),
            (
                "required_capabilities",
                spec.get("required_capabilities").clone(),
            ),
        ]))
    }
}

fn identifier(value: &Value, key: &str) -> Result<()> {
    require(
        receipt_id(value.get(key).str()?, 160),
        format!("invalid team {key}"),
    )
}
fn hash(value: &Value, key: &str) -> Result<()> {
    require(
        json::hex(value.get(key).str()?, 64),
        format!("invalid team {key}"),
    )
}
fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && Path::new(value)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
fn strings(value: &Value, limit: usize) -> Result<()> {
    let values = value.strings()?;
    require(
        values.len() <= limit
            && values.iter().all(|v| receipt_id(v, 160))
            && values.iter().copied().collect::<BTreeSet<_>>().len() == values.len(),
        "invalid or duplicate team resource/capability reference",
    )
}

impl MemberBinding {
    pub fn parse(value: &Value) -> Result<Self> {
        json::keys(
            value,
            &[
                "id",
                "generation",
                "display_name",
                "logical_parent",
                "root_owner",
                "role",
                "access",
                "route",
                "attempt_id",
                "worktree_ref",
                "native_identity",
            ],
            &[],
        )?;
        for key in ["id", "logical_parent", "root_owner"] {
            identifier(value, key)?;
        }
        require(
            value.get("generation").int()? > 0,
            "invalid member generation",
        )?;
        let display = value.get("display_name").str()?;
        require(display.chars().count() <= 160, "member label exceeds bound")?;
        plain(display, "member label")?;
        require(
            ["lead", "worker", "reviewer", "verifier"].contains(&value.text("role")),
            "invalid team role",
        )?;
        require(
            ["read", "write"].contains(&value.text("access")),
            "invalid team access",
        )?;
        require(
            value.text("role") != "lead" || value.text("access") == "read",
            "team leads are read-only",
        )?;
        // TM-01 registers unlaunched members. Later enrollment must prove actual attempts.
        require(
            value.get("attempt_id") == &Value::Null && value.get("worktree_ref") == &Value::Null,
            "planned members cannot claim an existing execution",
        )?;
        let route = value.get("route");
        json::keys(
            route,
            &[
                "schema",
                "harness",
                "provider",
                "transport",
                "account",
                "host",
                "configured_model",
                "requested_model",
                "resolved_model",
                "served_model",
                "requested_effort",
                "applied_effort",
                "model_family",
                "policy_sha256",
                "catalog_sha256",
                "capability_sha256",
            ],
            &[],
        )?;
        require(
            route.get("schema").is_int(1),
            "unsupported team route schema",
        )?;
        for key in ["harness", "provider", "transport", "host"] {
            identifier(route, key)?;
        }
        for key in [
            "account",
            "configured_model",
            "requested_model",
            "resolved_model",
            "requested_effort",
            "model_family",
        ] {
            if route.get(key) != &Value::Null {
                let text = route.get(key).str()?;
                require(
                    !text.is_empty() && text.chars().count() <= 160,
                    "invalid route identity",
                )?;
                plain(text, "route identity")?;
            }
        }
        // A contract cannot manufacture observations before a physical call exists.
        require(
            route.get("served_model") == &Value::Null
                && route.get("applied_effort") == &Value::Null
                && value.get("native_identity") == &Value::Null,
            "planned route contains execution observations",
        )?;
        hash(route, "policy_sha256")?;
        for key in ["catalog_sha256", "capability_sha256"] {
            if route.get(key) != &Value::Null {
                hash(route, key)?;
            }
        }
        Ok(Self {
            value: value.clone(),
        })
    }
}

impl TeamSpec {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        require(raw.len() <= MAX_SPEC_BYTES, "team contract exceeds bound")?;
        screen(raw)?;
        let value = json::parse_bounded(raw, MAX_SPEC_BYTES)?;
        json::keys(
            &value,
            &[
                "schema",
                "kind",
                "id",
                "generation",
                "previous_sha256",
                "root_owner",
                "root_owner_generation",
                "room_id",
                "plan_id",
                "task_id",
                "task_sha256",
                "objective_sha256",
                "criterion_sha256",
                "base_sha",
                "mode",
                "allowed_paths",
                "forbidden_paths",
                "resources",
                "policy_ref",
                "policy_sha256",
                "budget_ref",
                "expires_at",
                "max_members",
                "required_capabilities",
                "members",
            ],
            &[],
        )?;
        require(
            value.get("schema").is_int(1) && value.text("kind") == "oms-team-spec",
            "unsupported team contract schema",
        )?;
        require(
            valid_id(value.text("id")) && value.text("id").len() <= 160,
            "invalid team id",
        )?;
        for key in [
            "root_owner",
            "root_owner_generation",
            "room_id",
            "plan_id",
            "task_id",
        ] {
            identifier(&value, key)?;
        }
        let generation = value.get("generation").int()?;
        require(
            generation > 0 && generation <= i128::from(u64::MAX),
            "invalid team generation",
        )?;
        require(
            if generation == 1 {
                value.get("previous_sha256").str()?.is_empty()
            } else {
                json::hex(value.get("previous_sha256").str()?, 64)
            },
            "broken team predecessor",
        )?;
        for key in [
            "task_sha256",
            "objective_sha256",
            "criterion_sha256",
            "policy_sha256",
        ] {
            hash(&value, key)?;
        }
        require(
            crate::repository::commit(value.get("base_sha").str()?),
            "invalid team base",
        )?;
        require(
            ["single", "oms-team", "native-team"].contains(&value.text("mode")),
            "unsupported team mode",
        )?;
        for key in ["allowed_paths", "forbidden_paths"] {
            let paths = value
                .get(key)
                .strings()?
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
            require(
                clean_paths(&paths)? == paths
                    && paths.iter().collect::<BTreeSet<_>>().len() == paths.len(),
                "team paths must be normalized and unique",
            )?;
        }
        strings(value.get("resources"), 128)?;
        strings(value.get("required_capabilities"), 128)?;
        require(
            relative(value.get("policy_ref").str()?),
            "invalid team policy reference",
        )?;
        if value.get("budget_ref") != &Value::Null {
            require(
                relative(value.get("budget_ref").str()?),
                "invalid team budget reference",
            )?;
        }
        require(
            parse_ts(value.get("expires_at").str()?).is_some(),
            "invalid team expiry",
        )?;
        let members = value.get("members").array()?;
        let bound = value.get("max_members").int()?;
        require(
            (1..=MAX_MEMBERS as i128).contains(&bound)
                && !members.is_empty()
                && members.len() as i128 <= bound,
            "team member bound exceeded",
        )?;
        let mut by_id = BTreeMap::new();
        let owner = value.text("root_owner");
        let mut leads = 0;
        for member in members {
            MemberBinding::parse(member)?;
            let id = member.text("id");
            require(
                id != owner && by_id.insert(id, member).is_none(),
                "duplicate member or root-owner cycle",
            )?;
            require(
                member.text("root_owner") == owner,
                "foreign member root owner",
            )?;
            require(
                member.get("route").get("policy_sha256") == value.get("policy_sha256"),
                "member policy differs from team",
            )?;
            if member.text("role") == "lead" {
                leads += 1;
            }
        }
        require(leads == 1, "team requires exactly one logical lead")?;
        for member in members {
            let parent = member.text("logical_parent");
            if member.text("role") == "lead" {
                require(
                    parent == owner,
                    "lead must descend directly from the root owner",
                )?;
            } else {
                require(
                    by_id.get(parent).is_some_and(|p| p.text("role") == "lead"),
                    "member lineage is cyclic, too deep or foreign",
                )?;
            }
        }
        Ok(Self { value })
    }
}

pub fn criterion_digest(plan: &Value, task: &Value) -> Result<String> {
    json::hash(&object([
        ("accept", plan.get("accept").clone()),
        ("verify", task.get("verify").clone()),
    ]))
}

// One command sees each shared producer once. This cache is invocation-local;
// the next command revalidates current bytes and never reads cached authority.
struct Bindings<'a, 'p> {
    plan: &'a Plan<'p>,
    lifecycle: Option<Result<(Value, Vec<Value>)>>,
    rooms: BTreeMap<String, Result<Value>>,
    policies: BTreeMap<String, Result<String>>,
    head: Option<Result<String>>,
}
impl<'a, 'p> Bindings<'a, 'p> {
    fn new(plan: &'a Plan<'p>) -> Self {
        Self {
            plan,
            lifecycle: None,
            rooms: BTreeMap::new(),
            policies: BTreeMap::new(),
            head: None,
        }
    }
    fn head(&mut self) -> Result<&str> {
        self.head
            .get_or_insert_with(|| self.plan.git().text(&["rev-parse", "HEAD"]))
            .as_ref()
            .map(String::as_str)
            .map_err(Clone::clone)
    }
    fn policy(&mut self, path: &str) -> Result<&str> {
        self.policies
            .entry(path.into())
            .or_insert_with(|| self.plan.source.read(path, MAX_SPEC_BYTES).map(json::sha))
            .as_ref()
            .map(String::as_str)
            .map_err(Clone::clone)
    }
    fn owner(&mut self, spec: &Value) -> Result<()> {
        let plan = self.plan;
        let (attempts, events) = self
            .lifecycle
            .get_or_insert_with(|| {
                let raw = plan.store.read(".oms/lifecycle/events.jsonl", MAX_STREAM)?;
                Ok((
                    crate::provenance::lifecycle_projection(plan, &raw)?,
                    rows(&raw)?,
                ))
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let root = attempts.get(spec.text("root_owner"));
        require(
            root.text("tool") == "panel-main"
                && root.text("parent_attempt_id").is_empty()
                && root.get("terminal") == &Value::Bool(false)
                && ["working", "blocked"].contains(&root.text("state"))
                && root.get("refs").text("panel_room_id") == spec.text("room_id"),
            "team root owner is stale or foreign",
        )?;
        require(
            events.iter().any(|e| {
                e.text("attempt_id") == spec.text("root_owner")
                    && e.text("event_type") == "attempt.created"
                    && e.text("event_id") == spec.text("root_owner_generation")
            }),
            "team owner generation changed",
        )?;
        require(
            attempts
                .object()?
                .iter()
                .filter(|(_, a)| {
                    a.text("tool") == "panel-main"
                        && a.get("terminal") == &Value::Bool(false)
                        && a.get("refs").text("panel_room_id") == spec.text("room_id")
                })
                .count()
                == 1,
            "team room has multiple root owners",
        )?;
        let room_id = spec.text("room_id");
        let room = self
            .rooms
            .entry(room_id.into())
            .or_insert_with(|| {
                let raw = plan
                    .store
                    .read(format!(".oms/threads/{room_id}.jsonl"), MAX_STREAM)?;
                crate::provenance::room_projection(plan, room_id, &raw)
            })
            .as_ref()
            .map_err(Clone::clone)?;
        require(
            room.get("closed") == &Value::Bool(false)
                && room.get("participants").array()?.iter().any(|p| {
                    p.text("participant") == spec.text("root_owner")
                        && p.text("role") == "main"
                        && p.get("joined") == &Value::Bool(true)
                }),
            "team root owner is not enrolled in the current room",
        )?;
        Ok(())
    }
}

fn current(bindings: &mut Bindings<'_, '_>, s: &Snapshot, spec: &Value) -> Result<()> {
    let task = s.task(spec.text("task_id"))?;
    require(
        s.contract.get("satisfied") == &Value::Bool(true),
        "team parent project contract changed",
    )?;
    require(
        s.plan.get("plan_id") == spec.get("plan_id")
            && json::hash(task)? == spec.text("task_sha256")
            && json::sha(s.plan.get("goal").str()?.as_bytes()) == spec.text("objective_sha256")
            && criterion_digest(&s.plan, task)? == spec.text("criterion_sha256"),
        "team task or criterion changed",
    )?;
    require(
        !["done", "cancelled", "landing"].contains(&task.text("state")),
        "team task is terminal or landing",
    )?;
    let allowed_paths = clean_paths(
        &task
            .get("allowed_paths")
            .strings()?
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )?;
    let allowed = if allowed_paths.is_empty() {
        vec!["."]
    } else {
        allowed_paths.iter().map(String::as_str).collect::<Vec<_>>()
    };
    let forbidden = spec.get("forbidden_paths").strings()?;
    let declared = spec.get("allowed_paths").strings()?;
    let declared = if declared.is_empty() {
        vec!["."]
    } else {
        declared
    };
    for path in declared {
        require(
            oms_contract_markdown::scope::pattern_within_envelope(path, &allowed).map_err(error)?,
            "team scope exceeds parent task",
        )?;
    }
    for path in task.get("forbidden_paths").strings()? {
        let path = crate::engine::clean_path(path)?;
        require(
            oms_contract_markdown::scope::pattern_within_envelope(&path, &forbidden)
                .map_err(error)?,
            "team drops a parent forbidden scope",
        )?;
    }
    require(
        bindings.head()? == spec.text("base_sha"),
        "team base changed",
    )?;
    require(
        bindings.policy(spec.text("policy_ref"))? == spec.text("policy_sha256"),
        "team policy changed",
    )?;
    require(
        parse_ts(spec.text("expires_at")) > parse_ts(&bindings.plan.context.ts),
        "team contract expired",
    )?;
    bindings.owner(spec)
}

fn versions(plan: &Plan<'_>, id: &str, record: &Value) -> Result<Vec<Value>> {
    json::keys(record, &["versions", "phase", "reason"], &[])?;
    require(
        ["planned", "held", "settled"].contains(&record.text("phase")),
        "unsupported team lifecycle phase",
    )?;
    let reason = record.get("reason").str()?;
    require(
        reason.chars().count() <= 300,
        "stored team reason exceeds bound",
    )?;
    plain(reason, "stored team reason")?;
    let refs = record.get("versions").array()?;
    require(
        !refs.is_empty() && refs.len() <= 256,
        "invalid team history bound",
    )?;
    let mut out = Vec::new();
    let mut previous = String::new();
    for (i, reference) in refs.iter().enumerate() {
        json::keys(reference, &["path", "sha256"], &[])?;
        hash(reference, "sha256")?;
        let path = format!(".oms/plan/team-specs/{}.json", reference.text("sha256"));
        require(
            reference.text("path") == path,
            "foreign team artifact reference",
        )?;
        let raw = plan.store.read(&path, MAX_SPEC_BYTES)?;
        require(
            json::sha(&raw) == reference.text("sha256"),
            "team artifact digest mismatch",
        )?;
        let spec = TeamSpec::parse(&raw)?.value;
        require(
            spec.text("id") == id
                && spec.get("generation").is_int((i + 1) as i128)
                && spec.text("previous_sha256") == previous,
            "broken team history",
        )?;
        previous = reference.text("sha256").into();
        out.push(spec);
    }
    Ok(out)
}

fn view(
    bindings: &mut Bindings<'_, '_>,
    s: &Snapshot,
    id: &str,
    record: &Value,
    spec: &Value,
) -> Result<Value> {
    let refusal = current(bindings, s, spec).err();
    Ok(object([
        ("id", string(id)),
        ("spec", spec.clone()),
        ("history", record.get("versions").clone()),
        (
            "phase",
            string(if refusal.is_some() && record.text("phase") != "settled" {
                "held"
            } else {
                record.text("phase")
            }),
        ),
        (
            "reason",
            string(refusal.map_or_else(|| record.text("reason").to_owned(), |e| e.message)),
        ),
        ("usage_coverage", Value::Null),
        ("cleanup_coverage", Value::Null),
        ("verified", Value::Null),
        ("admitted", Value::Null),
        ("landed", Value::Null),
        ("published", Value::Null),
    ]))
}

fn registry(plan: &Plan<'_>, s: &Snapshot) -> Result<(Value, BTreeMap<String, Value>, usize)> {
    let teams = s.plan.opt("teams").cloned().unwrap_or_else(empty);
    require(
        teams.object()?.len() <= MAX_TEAMS,
        "team registry exceeds bound",
    )?;
    let retained = teams
        .object()?
        .iter()
        .try_fold(0usize, |sum, (_, record)| {
            sum.checked_add(record.get("versions").array()?.len())
                .ok_or_else(|| error("team history count overflow"))
        })?;
    require(retained <= 256, "team history exceeds aggregate bound")?;
    let mut heads = BTreeMap::new();
    // Validate retained history even when only one head is requested.
    for (id, record) in teams.object()? {
        let id = std::str::from_utf8(id).map_err(|_| error("invalid team id"))?;
        require(valid_id(id) && id.len() <= 160, "invalid stored team id")?;
        let specs = versions(plan, id, record)?;
        heads.insert(id.to_owned(), specs.last().unwrap().clone());
    }
    Ok((teams, heads, retained))
}

pub(crate) fn execute(plan: &Plan<'_>, req: &Request) -> Result<Response> {
    let mut s = plan.load()?;
    let (mut teams, heads, retained) = registry(plan, &s)?;
    let mut bindings = Bindings::new(plan);
    if req.action == Action::TeamList {
        let values = teams
            .object()?
            .iter()
            .map(|(id, record)| {
                let id = std::str::from_utf8(id).unwrap();
                view(&mut bindings, &s, id, record, &heads[id])
            })
            .collect::<Result<Vec<_>>>()?;
        return Response::json(&Value::Array(values));
    }
    if req.action == Action::TeamShow {
        let id = req.value("id")?;
        return Response::json(&view(
            &mut bindings,
            &s,
            id,
            teams.opt(id).ok_or_else(|| error("no such team"))?,
            &heads[id],
        )?);
    }
    if req.action == Action::TeamCancel {
        let id = req.value("id")?;
        let mut record = teams
            .opt(id)
            .cloned()
            .ok_or_else(|| error("no such team"))?;
        let spec = &heads[id];
        require(
            plan.context.panel_main_attempt == spec.text("root_owner")
                && plan.context.room_id == spec.text("room_id"),
            "team cancellation belongs to another main",
        )?;
        bindings.owner(spec)?;
        let reference = record.get("versions").array()?.last().unwrap();
        require(
            req.value("expected-team-sha256")? == reference.text("sha256"),
            "team cancellation head changed",
        )?;
        let reason = req.value("reason")?;
        plain(reason, "team cancellation reason")?;
        require(reason.chars().count() <= 300, "team reason exceeds bound")?;
        screen(reason.as_bytes())?;
        record.set("phase", string("settled"))?;
        record.set("reason", string(reason))?;
        teams.set(id, record)?;
    } else {
        let raw = plan.source.read(req.value("team-spec")?, MAX_SPEC_BYTES)?;
        let spec = TeamSpec::parse(&raw)?.value;
        let id = spec.text("id");
        require(
            plan.context.panel_main_attempt == spec.text("root_owner")
                && plan.context.room_id == spec.text("room_id"),
            "team registration belongs to another main",
        )?;
        current(&mut bindings, &s, &spec)?;
        let mut record = teams.opt(id).cloned().unwrap_or_else(|| {
            object([
                ("versions", Value::Array(vec![])),
                ("phase", string("planned")),
                ("reason", string("")),
            ])
        });
        let mut refs = record.get("versions").array()?.to_vec();
        let expected = refs.last().map_or("", |r| r.text("sha256")).to_owned();
        require(
            req.text("expected-team-sha256")? == expected
                && spec.text("previous_sha256") == expected
                && spec.get("generation").is_int((refs.len() + 1) as i128),
            "team generation or predecessor changed",
        )?;
        require(
            retained < 256 && (teams.opt(id).is_some() || teams.object()?.len() < MAX_TEAMS),
            "team history exceeds bound",
        )?;
        let canonical = json::canonical(&spec)?;
        let (reference, _) =
            plan.store
                .immutable(".oms/plan/team-specs", &canonical, ".json", MAX_SPEC_BYTES)?;
        refs.push(reference);
        record.set("versions", Value::Array(refs))?;
        record.set(
            "phase",
            string(if expected.is_empty() {
                "planned"
            } else {
                "held"
            }),
        )?;
        record.set(
            "reason",
            string(if expected.is_empty() {
                ""
            } else {
                "new generation requires admission"
            }),
        )?;
        teams.set(id, record)?;
    }
    s.plan.set("teams", teams)?;
    plan.save(&mut s)?;
    Response::json(s.plan.get("teams"))
}
