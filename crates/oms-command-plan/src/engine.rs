use crate::json::{self, Value, empty, list, number, object, string};
use crate::repository::Git;
use crate::services::Services;
use crate::storage::{self, ACTIVE, MAX_JSON, MAX_PATCH, Store};
use crate::{Action, Request, Response, Result, drift, error, require};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_general_category::{GeneralCategory, get_general_category};

pub const STATES: &[&str] = &[
    "ready",
    "claimed",
    "running",
    "review",
    "landing",
    "blocked",
    "done",
    "cancelled",
];
/// A caller's issued task snapshot, before any provider call. It cannot select
/// the current claimant by task id or grant completion/landing authority.
#[derive(Clone, Debug)]
pub struct DelegateBinding(Value);
impl DelegateBinding {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let value = json::parse_bounded(raw, 4096)?;
        json::keys(
            &value,
            &[
                "schema",
                "plan_id",
                "task_id",
                "task_sha256",
                "lease_id",
                "lease_epoch",
                "base_sha",
            ],
            &[],
        )?;
        require(
            value.get("schema").is_int(1)
                && lineage(value.text("plan_id"), "plan_")
                && valid_id(value.text("task_id"))
                && value.text("task_id").len() <= 160
                && json::hex(value.text("task_sha256"), 64)
                && lineage(value.text("lease_id"), "lease_")
                && value.get("lease_epoch").int().is_ok_and(|n| n > 0)
                && [40, 64]
                    .iter()
                    .any(|&n| json::hex(value.text("base_sha"), n)),
            "invalid exact Plan delegation binding",
        )?;
        Ok(Self(value))
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        json::canonical(&self.0)
    }
    pub fn task_id(&self) -> &str {
        self.0.text("task_id")
    }
    pub fn lease_id(&self) -> &str {
        self.0.text("lease_id")
    }
    pub fn base_sha(&self) -> &str {
        self.0.text("base_sha")
    }
}
const LANDING_REVIEW_SHA256: &str = "landing_review_sha256";

/// Comparison target for landing the exact Plan review selected by a runner.
#[derive(Clone, Debug)]
pub struct ReviewFence {
    lease: DelegateBinding,
    report_sha256: String,
    patch_sha256: String,
}
impl ReviewFence {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let row = json::parse_bounded(raw, 4096)?;
        json::keys(&row, &["binding", "report_sha256", "patch_sha256"], &[])?;
        let lease = DelegateBinding::parse(&json::canonical(row.get("binding"))?)?;
        let report_sha256 = row.text("report_sha256").to_owned();
        let patch_sha256 = row.text("patch_sha256").to_owned();
        require(
            json::hex(&report_sha256, 64) && json::hex(&patch_sha256, 64),
            "invalid Plan review evidence digest",
        )?;
        Ok(Self {
            lease,
            report_sha256,
            patch_sha256,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        json::canonical(&object([
            ("binding", self.lease.0.clone()),
            ("report_sha256", string(&self.report_sha256)),
            ("patch_sha256", string(&self.patch_sha256)),
        ]))
    }
    pub fn task_id(&self) -> &str {
        self.lease.task_id()
    }
    pub(crate) fn lease_id(&self) -> &str {
        self.lease.lease_id()
    }
    pub(crate) fn plan_id(&self) -> &str {
        self.lease.0.text("plan_id")
    }
    pub fn base_sha(&self) -> &str {
        self.lease.base_sha()
    }
    pub fn patch_sha256(&self) -> &str {
        &self.patch_sha256
    }
    fn digest(&self) -> Result<String> {
        Ok(json::sha(self.encode()?))
    }
    /// Include the review binding in the native Plan receipt without changing
    /// the outer landing ledger digest shared by legacy consumers.
    pub fn stamp_landing_task(&self, task: &mut Value) -> Result<()> {
        task.set(LANDING_REVIEW_SHA256, string(self.digest()?))
    }
    fn require_landing_stamp(&self, task: &Value) -> Result<()> {
        require(
            task.text(LANDING_REVIEW_SHA256) == self.digest()?,
            "Plan landing lacks the exact review binding",
        )
    }
    fn require_task(&self, plan: &Value, task: &Value, state: &str) -> Result<()> {
        require(
            plan.get("plan_id") == self.lease.0.get("plan_id")
                && task.text("id") == self.task_id()
                && task.text("state") == state
                && task.text("review_lease_id") == self.lease.lease_id()
                && task.get("lease_id") == self.lease.0.get("lease_id")
                && task.get("lease_epoch") == self.lease.0.get("lease_epoch"),
            "Plan landing task or reviewed lease changed",
        )
    }
}
#[derive(Clone)]
pub struct Context {
    /// Caller directory that relative request operands resolve against.
    pub cwd: PathBuf,
    pub execution_repo: PathBuf,
    pub state_repo: PathBuf,
    pub plan_file: PathBuf,
    pub ts: String,
    pub child: bool,
    pub delegate_depth: String,
    pub owner_id: String,
    pub room_id: String,
    pub participant: String,
    pub panel_main_attempt: String,
    pub attempt_id: String,
    pub harness_call_id: String,
    pub claim_ttl: u64,
    pub run_id: String,
    pub cycle: Option<u64>,
    pub lock_settings: oms_lock::Settings,
}
impl Context {
    pub fn from_environment(req: &Request, default_repo: &Path) -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|_| error("cannot resolve working directory"))?;
        Self::from_environment_at(req, default_repo, &cwd, None)
    }
    /// Same as `from_environment`, but relative operands and `OMS_STATE_REPO`
    /// resolve against `cwd`, and `state_hint` names the shared state root
    /// (a verified scratch owner) used when `OMS_STATE_REPO` is unset.
    pub fn from_environment_at(
        req: &Request,
        default_repo: &Path,
        cwd: &Path,
        state_hint: Option<&Path>,
    ) -> Result<Self> {
        Self::resolve(req, default_repo, cwd, &AtomicBool::new(false), &|_| {
            Ok(state_hint.map(Path::to_path_buf))
        })
    }
    /// Resolves the same parent environment and physical roots while sharing
    /// the caller's cancellation and absolute admission deadline, including Git.
    pub fn from_environment_before(
        req: &Request,
        default_repo: &Path,
        cwd: &Path,
        state_hint: Option<&Path>,
        interrupted: &AtomicBool,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let result = Self::resolve_inner(
            req,
            default_repo,
            cwd,
            interrupted,
            Some(deadline),
            &|_| Ok(state_hint.map(Path::to_path_buf)),
            None,
        );
        Self::before(interrupted, Some(deadline))?;
        result
    }
    /// A native caller has already selected execution and shared-state roots.
    /// Ambient locators cannot redirect this read; child and owner flags remain
    /// the actual process environment and grant no additional authority.
    pub fn from_selected_state_before(
        req: &Request,
        execution_repo: &Path,
        cwd: &Path,
        state_repo: &Path,
        interrupted: &AtomicBool,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let result = Self::resolve_inner(
            req,
            execution_repo,
            cwd,
            interrupted,
            Some(deadline),
            &|_| Ok(None),
            Some(state_repo),
        );
        Self::before(interrupted, Some(deadline))?;
        result
    }
    pub(crate) fn resolve(
        req: &Request,
        default_repo: &Path,
        cwd: &Path,
        interrupted: &AtomicBool,
        hint: &dyn Fn(&Path) -> Result<Option<PathBuf>>,
    ) -> Result<Self> {
        Self::resolve_inner(req, default_repo, cwd, interrupted, None, hint, None)
    }
    fn before(interrupted: &AtomicBool, deadline: Option<std::time::Instant>) -> Result<()> {
        if interrupted.load(Ordering::Acquire) {
            return Err(crate::Error {
                code: 130,
                message: "plan context resolution interrupted".into(),
            });
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(crate::Error {
                code: 75,
                message: "plan context resolution deadline expired".into(),
            });
        }
        Ok(())
    }
    fn resolve_inner(
        req: &Request,
        default_repo: &Path,
        cwd: &Path,
        interrupted: &AtomicBool,
        deadline: Option<std::time::Instant>,
        hint: &dyn Fn(&Path) -> Result<Option<PathBuf>>,
        selected_state: Option<&Path>,
    ) -> Result<Self> {
        use crate::cli::{physical_dir, physical_leaf, worktree_root_before};
        Self::before(interrupted, deadline)?;
        let get = |n: &str| std::env::var(n).unwrap_or_default();
        let repo = req.repo.as_deref().unwrap_or(default_repo);
        let execution_repo =
            worktree_root_before(&physical_dir(cwd, repo.as_os_str()), interrupted, deadline)?;
        let state = if let Some(selected) = selected_state {
            crate::cli::canonicalize_dir(&physical_dir(cwd, selected.as_os_str()))
                .map_err(|_| error("cannot resolve selected plan repository"))?
        } else {
            match std::env::var_os("OMS_STATE_REPO").filter(|s| !s.is_empty()) {
                Some(s) => worktree_root_before(&physical_dir(cwd, &s), interrupted, deadline)
                    .map_err(|_| error("cannot resolve plan repository"))?,
                None => match hint(&execution_repo)? {
                    Some(h) => crate::cli::canonicalize_dir(&h)
                        .map_err(|_| error("cannot resolve plan repository"))?,
                    None => execution_repo.clone(),
                },
            }
        };
        let state_repo = state;
        // The default plan path keeps its own spelling so the storage layer's
        // no-follow anchor still refuses a symlinked `.oms`, `plan` or file.
        let plan_file = match &req.file {
            Some(f) => physical_leaf(cwd, f.as_os_str()),
            None => state_repo.join(ACTIVE),
        };
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| error("invalid system time"))?;
        let ts = DateTime::<Utc>::from_timestamp(time.as_secs() as i64, 0)
            .ok_or_else(|| error("invalid system time"))?
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let claim_ttl = get("OMS_PLAN_CLAIM_TTL").parse().unwrap_or(3600);
        Ok(Self {
            cwd: cwd.to_path_buf(),
            execution_repo,
            state_repo,
            plan_file,
            ts,
            child: get("OMS_HARNESS_CHILD") == "1",
            delegate_depth: get("OMS_HARNESS_DELEGATE_DEPTH"),
            owner_id: get("OMS_AUTOPILOT_OWNER_ID"),
            room_id: get("OMS_ROOM_ID"),
            participant: get("OMS_ROOM_PARTICIPANT"),
            panel_main_attempt: get("OMS_PANEL_MAIN_ATTEMPT"),
            attempt_id: get("OMS_ATTEMPT_ID"),
            harness_call_id: get("OMS_HARNESS_CALL_ID"),
            claim_ttl,
            run_id: get("OMS_GOAL_RUN_ID"),
            cycle: get("OMS_GOAL_CYCLE").parse().ok(),
            lock_settings: oms_lock::Settings::from_env().map_err(|e| error(e.to_string()))?,
        })
    }
    pub fn parent(&self) -> Result<()> {
        require(
            !self.child && matches!(self.delegate_depth.as_str(), "" | "0"),
            "parent-only operation",
        )
    }
    pub fn canonical(&self) -> bool {
        self.plan_file == self.state_repo.join(ACTIVE)
    }
}

pub(crate) fn scheduling_metadata(
    task: &mut Value,
    resources: &[String],
    contracts: &[String],
    priority: i32,
) -> Result<()> {
    let mut resources = resources.to_vec();
    for token in &resources {
        require(
            !token.is_empty()
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b)),
            format!("invalid resource: {token}"),
        )?;
    }
    resources.sort();
    resources.dedup();
    for token in contracts {
        let (id, version) = token
            .split_once('@')
            .map_or((token.as_str(), None), |(id, v)| (id, Some(v)));
        require(
            valid_id(id)
                && version.is_none_or(|v| {
                    v.strip_prefix('v').is_some_and(|n| {
                        !n.is_empty()
                            && n.bytes().all(|b| b.is_ascii_digit())
                            && n.parse::<u64>().is_ok()
                    })
                }),
            format!("invalid contract reference: {token}"),
        )?;
    }
    task.set("resources", list(resources.iter().map(String::as_str)))?;
    task.set("contracts", list(contracts.iter().map(String::as_str)))?;
    task.set("priority", number(priority))
}

pub(crate) fn validate_graph(tasks: &[Value]) -> Result<()> {
    let mut graph = BTreeMap::new();
    for task in tasks {
        let id = task.text("id");
        require(!graph.contains_key(id), format!("duplicate task: {id}"))?;
        graph.insert(id, task.get("depends").strings()?);
    }
    for (id, deps) in &graph {
        for dep in deps {
            require(
                graph.contains_key(dep),
                format!("task {id} has unknown dependency {dep}"),
            )?;
        }
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for (id, deps) in &graph {
            if deps.iter().all(|dep| done.contains(dep)) {
                done.insert(*id);
            }
        }
        if done.len() == graph.len() {
            return Ok(());
        }
        if done.len() == before {
            let cycle = graph
                .keys()
                .filter(|id| !done.contains(*id))
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(error(format!("dependency cycle involving: {cycle}")));
        }
    }
}

pub struct Snapshot {
    pub plan: Value,
    pub raw: Option<Vec<u8>>,
    pub generation: Option<Value>,
    pub unverified: BTreeSet<String>,
    pub contract: Value,
    pub(crate) registry: Value,
    pub(crate) markers: Option<Vec<Value>>,
    pub(crate) settled: BTreeSet<String>,
    /// Lazily read local GitHub snapshot cache bytes; empty when absent.
    pub(crate) github_cache: Option<Vec<u8>>,
}
impl Snapshot {
    pub fn tasks(&self) -> Result<&[(Vec<u8>, Value)]> {
        self.plan.get("tasks").object()
    }
    pub fn task(&self, id: &str) -> Result<&Value> {
        self.plan
            .get("tasks")
            .opt(id)
            .ok_or_else(|| error(format!("no such task: {id}")))
    }
    pub fn put(&mut self, id: &str, task: Value) -> Result<()> {
        let mut tasks = self.plan.get("tasks").clone();
        tasks.set(id, task)?;
        self.plan.set("tasks", tasks)
    }
    pub fn verified_done(&self, task: &Value) -> bool {
        task.text("state") == "done" && !self.unverified.contains(task.text("id"))
    }
    pub fn deps_done(&self, task: &Value) -> bool {
        task.get("depends").strings().is_ok_and(|deps| {
            deps.iter()
                .all(|id| self.task(id).is_ok_and(|t| self.verified_done(t)))
        })
    }
    pub fn ordered(&self) -> Result<Vec<Value>> {
        let mut tasks = self
            .tasks()?
            .iter()
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| a.text("created").cmp(b.text("created")));
        Ok(tasks)
    }
    pub fn require_contract(&self) -> Result<()> {
        require(
            self.contract.get("satisfied") == &Value::Bool(true),
            format!(
                "PROJECT.md contract blocks new task authority: {}",
                self.contract.text("blocker")
            ),
        )
    }
}
pub struct Plan<'a> {
    pub context: Context,
    pub store: Store,
    pub source: Store,
    file_store: Option<Store>,
    pub services: &'a dyn Services,
    pub interrupted: &'a AtomicBool,
    pub(crate) proof_deadline: Option<std::time::Instant>,
    pub(crate) group_snapshot: Option<std::sync::Arc<crate::storage::GroupProofSnapshot>>,
}

impl<'a> Plan<'a> {
    pub fn open(
        context: Context,
        services: &'a dyn Services,
        interrupted: &'a AtomicBool,
    ) -> Result<Self> {
        require(
            parse_ts(&context.ts).is_some(),
            "noncanonical plan timestamp",
        )?;
        let store = Store::open(&context.state_repo)?
            .with_group_resolver(services.group_membership_resolver());
        let source = Store::open(&context.execution_repo)?
            .with_group_resolver(services.group_membership_resolver());
        let file_store = if context.plan_file.starts_with(&context.state_repo) {
            None
        } else {
            let mut parent = context
                .plan_file
                .parent()
                .ok_or_else(|| error("plan file has no parent"))?;
            while !parent.exists() {
                parent = parent
                    .parent()
                    .ok_or_else(|| error("plan file has no existing ancestor"))?;
            }
            Some(Store::open(parent)?.with_group_resolver(services.group_membership_resolver()))
        };
        require(
            store.repo() == context.state_repo && source.repo() == context.execution_repo,
            "repository spelling is not physical",
        )?;
        Ok(Self {
            context,
            store,
            source,
            file_store,
            services,
            interrupted,
            proof_deadline: None,
            group_snapshot: None,
        })
    }
    pub(crate) fn plan_storage(&self) -> &Store {
        self.file_store.as_ref().unwrap_or(&self.store)
    }
    pub(crate) fn progress_path(&self) -> Result<PathBuf> {
        Ok(self
            .context
            .plan_file
            .parent()
            .ok_or_else(|| error("plan file has no parent"))?
            .join("progress.jsonl"))
    }
    pub fn git(&self) -> Git<'_> {
        Git {
            group_snapshot: self.group_snapshot.clone(),
            deadline: self.proof_deadline,
            repo: &self.context.execution_repo,
            interrupted: self.interrupted,
            group_resolver: self.services.group_membership_resolver(),
        }
    }
    pub fn state_git(&self) -> Git<'_> {
        Git {
            group_snapshot: self.group_snapshot.clone(),
            deadline: self.proof_deadline,
            repo: &self.context.state_repo,
            interrupted: self.interrupted,
            group_resolver: self.services.group_membership_resolver(),
        }
    }
    pub fn load(&self) -> Result<Snapshot> {
        self.load_from_snapshot(self.read_snapshot()?)
    }
    fn read_snapshot(&self) -> Result<Option<storage::FileSnapshot>> {
        self.plan_storage()
            .snapshot(&self.context.plan_file, MAX_JSON, true)
    }
    /// Read the task against a stable Plan file while a sibling may publish
    /// Review. Hold the writer's guard only for the bounded file snapshot.
    pub fn show_task_with_locked_snapshot(&self, id: &str) -> Result<Response> {
        let req = Request::new(Action::Show).with("id", id);
        req.validate()?;
        self.query(
            &req,
            &mut self.load_from_snapshot(self.read_locked_snapshot()?)?,
        )
    }
    /// Snapshot the Plan under its writer guard while a sibling publishes Review.
    fn read_locked_snapshot(&self) -> Result<Option<storage::FileSnapshot>> {
        self.locked(false, None, || self.read_snapshot())
    }
    fn load_from_snapshot(&self, snap: Option<storage::FileSnapshot>) -> Result<Snapshot> {
        let raw = snap.as_ref().map(|s| s.raw.clone());
        let strict =
            self.context.canonical() && self.store.directory_exists(".oms/plan/completions")?;
        let mut d = match &raw {
            Some(raw) if strict => json::parse(raw)?,
            Some(raw) => json::parse_legacy(raw, MAX_JSON)?,
            None => object([
                ("schema", number(3)),
                ("goal", string("")),
                ("accept", string("")),
                ("tasks", empty()),
            ]),
        };
        d.object()?;
        d.default("tasks", empty())?;
        d.default("accept", string(""))?;
        d.set("schema", number(3))?;
        if let Some(id) = d.opt("plan_id") {
            require(
                lineage(id.str()?, "plan_"),
                "plan_id is malformed; refusing to replace immutable lineage",
            )?;
        }
        let mut normalized = empty();
        for (id, task) in d.get("tasks").object()? {
            let id = std::str::from_utf8(id).map_err(|_| error("task id is not UTF-8"))?;
            require(valid_id(id), "plan contains invalid task id")?;
            let mut task = task.clone();
            task.object()?;
            assignment(task.opt("assignment").unwrap_or(&empty()))?;
            for (name, value) in [
                ("lease_epoch", number(0)),
                ("lease_id", string("")),
                ("repair_count", number(0)),
                ("repair_artifact", string("")),
                ("executor_id", string("")),
                ("executor_soul_sha256", string("")),
                ("autopilot_owner_id", string("")),
            ] {
                task.default(name, value)?;
            }
            crate::decision::validate(&self.store, &task)?;
            normalized.set(id, task)?;
        }
        d.set("tasks", normalized)?;
        let contract = self.contract_verdict(&d)?;
        let mut s = Snapshot {
            plan: d,
            raw,
            generation: snap.map(|s| s.generation),
            unverified: BTreeSet::new(),
            contract,
            registry: crate::contracts::load(&self.store)?.0,
            markers: None,
            settled: BTreeSet::new(),
            github_cache: None,
        };
        let tasks = s.ordered()?;
        for task in tasks {
            if task.opt("completion_kind").is_some() || task.opt("completion_receipt").is_some() {
                require(
                    self.context.canonical(),
                    "completion references belong only to canonical active plan",
                )?;
                if crate::completion::validate_done(self, &s.plan, &task, true)?
                    == crate::completion::CompletionState::Unavailable
                {
                    s.unverified.insert(task.text("id").into());
                }
            }
        }
        Ok(s)
    }
    pub(crate) fn locked<T>(
        &self,
        markers: bool,
        room: Option<&str>,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut guards = Vec::new();
        let settings = &self.context.lock_settings;
        let mut lock = |path: PathBuf| -> Result<()> {
            let authority = oms_lock::Lock::for_file(&path, settings.clone())
                .map_err(|e| error(e.to_string()))?;
            guards.push(authority.acquire(None).map_err(|e| crate::Error {
                code: e.exit_code(),
                message: e.to_string(),
            })?);
            Ok(())
        };
        if let Some(room) = room {
            require(room_id(room), "invalid completion room identity")?;
            lock(self.context.state_repo.join(".oms/landings.jsonl"))?;
            lock(
                self.context
                    .state_repo
                    .join(format!(".oms/threads/{room}.jsonl")),
            )?;
        }
        if markers || room.is_some() {
            lock(
                self.context
                    .state_repo
                    .join(".oms/delegations/.marker-set-lock-target"),
            )?;
        }
        lock(self.context.state_repo.join(ACTIVE))?;
        if !self.context.canonical() {
            lock(self.context.plan_file.clone())?;
        }
        let _lifecycle = if room.is_some() {
            Some(self.services.lifecycle_writer(&self.context.state_repo)?)
        } else {
            None
        };
        self.checkpoint()?;
        f()
    }
    pub(crate) fn checkpoint(&self) -> Result<()> {
        require(
            !self.interrupted.load(Ordering::Relaxed),
            "plan operation interrupted",
        )
    }
    pub(crate) fn save(&self, s: &mut Snapshot) -> Result<()> {
        let cleanup = if self.context.canonical()
            && !self
                .store
                .names(".oms/plan/completion-pin-leases", 8192, true)?
                .is_empty()
        {
            crate::completion::prepare_settled(self, &s.settled)?
        } else {
            vec![]
        };
        let raw = json::pretty(&s.plan)?;
        self.checkpoint()?;
        self.plan_storage().mutate(
            &self.context.plan_file,
            s.raw.as_deref().or(Some(&[])),
            &raw,
            MAX_JSON,
            true,
        )?;
        s.raw = Some(raw);
        for (path, raw) in cleanup {
            self.store
                .remove(&path, &raw, None)
                .map_err(|e| error(format!("plan change already published; {}", e.message)))?;
        }
        s.settled.clear();
        Ok(())
    }
    /// Check one immutable issued lease using the current canonical snapshot.
    /// Callers reopen Context before each check so expiry never uses launch time.
    pub fn check_delegate_binding(&self, binding: &DelegateBinding) -> Result<Value> {
        self.context.parent()?;
        self.locked(false, None, || {
            let mut snapshot = self.load()?;
            self.delegate_task(&mut snapshot, binding)
        })
    }

    /// Check the already-started provider leg before preparing its physical launch.
    pub fn check_running_delegate(&self, binding: &DelegateBinding, provider: &str) -> Result<()> {
        self.context.parent()?;
        self.locked(false, None, || {
            self.running_delegate(&mut self.load()?, binding, provider)
                .map(drop)
        })
    }

    /// Retain the canonical Plan lock across worker checkout proof and spawn.
    /// The caller supervises the child after release.
    pub fn with_running_delegate_before<T, E>(
        self,
        binding: &DelegateBinding,
        provider: &str,
        deadline: std::time::Instant,
        check_checkout: impl FnMut() -> std::result::Result<(), E>,
        spawn: impl FnOnce() -> std::result::Result<T, E>,
    ) -> Result<std::result::Result<T, E>> {
        self.with_running_delegate_before_clock(
            binding,
            provider,
            deadline,
            check_checkout,
            spawn,
            delegation_time,
        )
    }

    fn with_running_delegate_before_clock<T, E>(
        self,
        binding: &DelegateBinding,
        provider: &str,
        deadline: std::time::Instant,
        mut check_checkout: impl FnMut() -> std::result::Result<(), E>,
        spawn: impl FnOnce() -> std::result::Result<T, E>,
        now: impl Fn() -> Result<String>,
    ) -> Result<std::result::Result<T, E>> {
        let (plan, guard, groups) =
            self.delegate_writer_before(deadline, oms_lock::Lock::try_acquire)?;
        loop {
            plan.recheck_delegate_groups(&groups, deadline)?;
            let checked = plan.load().and_then(|mut snapshot| {
                let task = plan.running_delegate(&mut snapshot, binding, provider)?;
                Ok((snapshot, task))
            });
            if plan.resolve_delegate_group(&groups, deadline)? {
                continue;
            }
            let (mut snapshot, task) = checked?;
            guard.check().map_err(delegate_lock_error)?;
            Context::before(plan.interrupted, Some(deadline))?;
            if let Err(error) = check_checkout() {
                return Ok(Err(error));
            }
            Context::before(plan.interrupted, Some(deadline))?;
            // Marker custody can change independently of the Plan writer.
            // expired_at reloads that evidence only once the lease reaches its TTL.
            snapshot.markers = None;
            let expired = plan.expired_at(&mut snapshot, &task, true, &now()?);
            if plan.resolve_delegate_group(&groups, deadline)? {
                // Refresh all Plan and checkout proof after the account lookup.
                continue;
            }
            require(
                !expired?,
                "Plan delegation lease expired before provider start",
            )?;
            guard.check().map_err(delegate_lock_error)?;
            Context::before(plan.interrupted, Some(deadline))?;
            return Ok(spawn());
        }
    }

    fn delegate_writer_before(
        mut self,
        deadline: std::time::Instant,
        acquire: impl FnOnce(&oms_lock::Lock) -> std::result::Result<oms_lock::Guard, oms_lock::Error>,
    ) -> Result<(
        Self,
        oms_lock::Guard,
        std::sync::Arc<crate::storage::GroupProofSnapshot>,
    )> {
        self.context.parent()?;
        require(
            self.context.canonical(),
            "delegation requires the canonical Plan",
        )?;
        Context::before(self.interrupted, Some(deadline))?;
        let mut settings = self.context.lock_settings.clone();
        settings.timeout = settings
            .timeout
            .min(deadline.saturating_duration_since(std::time::Instant::now()));
        Context::before(self.interrupted, Some(deadline))?;
        let authority = oms_lock::Lock::for_file(&self.context.state_repo.join(ACTIVE), settings)
            .map_err(delegate_lock_error)?;
        let guard = acquire(&authority).map_err(delegate_lock_error)?;

        self.proof_deadline = Some(deadline);
        let groups = std::sync::Arc::new(crate::storage::GroupProofSnapshot::new());
        self.group_snapshot = Some(groups.clone());
        self.store = self.store.with_group_snapshot(Some(groups.clone()));
        self.source = self.source.with_group_snapshot(Some(groups.clone()));
        Ok((self, guard, groups))
    }

    fn recheck_delegate_groups(
        &self,
        groups: &crate::storage::GroupProofSnapshot,
        deadline: std::time::Instant,
    ) -> Result<()> {
        Context::before(self.interrupted, Some(deadline))?;
        for proof in groups.proofs()? {
            let current =
                self.services
                    .group_membership_before(proof.gid, deadline, self.interrupted)?;
            require(
                current == proof,
                "native group ownership changed before provider start",
            )?;
        }
        Ok(())
    }

    fn resolve_delegate_group(
        &self,
        groups: &crate::storage::GroupProofSnapshot,
        deadline: std::time::Instant,
    ) -> Result<bool> {
        let Some(gid) = groups.missing()? else {
            return Ok(false);
        };
        let proof = self
            .services
            .group_membership_before(gid, deadline, self.interrupted)?;
        groups.insert(gid, proof)?;
        Ok(true)
    }

    fn running_delegate(
        &self,
        snapshot: &mut Snapshot,
        binding: &DelegateBinding,
        provider: &str,
    ) -> Result<Value> {
        let task = self.delegate_task(snapshot, binding)?;
        require(
            task.text("state") == "running" && task.text("provider") == provider,
            "Plan provider launch no longer owns its exact running lease",
        )?;
        Ok(task)
    }

    /// Only Start and Review cross this adapter. The ordinary execute request,
    /// parent/context guards, locks and mutation checks are retained below.
    pub fn execute_bound_task(
        &self,
        req: &Request,
        binding: &DelegateBinding,
        index: Option<&Value>,
    ) -> Result<DelegateBinding> {
        self.context.parent()?;
        require(
            matches!(req.action, Action::Start | Action::Review)
                && req.value("id")? == binding.task_id()
                && req.value("lease-id")? == binding.lease_id()
                && (req.action == Action::Review) == index.is_some(),
            "bound delegation permits only its exact Start or Review",
        )?;
        let response = self.execute_inner(req, Some(binding), index)?;
        require(response.code == 0, "bound Plan mutation did not complete")?;
        DelegateBinding::parse(&response.stdout)
    }

    /// Validate and publish just the issued Start, with no opportunistic maintenance.
    pub fn execute_bound_start_before<T>(
        self,
        binding: &DelegateBinding,
        deadline: std::time::Instant,
        checkout: impl Fn(std::time::Instant) -> Result<T>,
        recheck_checkout: impl Fn(&T) -> Result<()>,
    ) -> Result<DelegateBinding> {
        let (plan, guard, groups) =
            self.delegate_writer_before(deadline, |lock| lock.acquire(None))?;
        let req = Request::new(Action::Start)
            .with("id", binding.task_id())
            .with("lease-id", binding.lease_id());
        loop {
            plan.recheck_delegate_groups(&groups, deadline)?;
            let checked = crate::retirement::completed_cleanup(&plan).and_then(|_| {
                let mut snapshot = plan.load()?;
                let task = plan.delegate_task(&mut snapshot, binding)?;
                Ok((snapshot, task))
            });
            if plan.resolve_delegate_group(&groups, deadline)? {
                continue;
            }
            let (mut snapshot, task) = checked?;
            guard.check().map_err(delegate_lock_error)?;
            Context::before(plan.interrupted, Some(deadline))?;
            let mut running = task.clone();
            plan.start_task(&req, &mut running)?;
            running.set("updated", string(&plan.context.ts))?;
            let mut next = binding.0.clone();
            next.set("task_sha256", string(json::hash(&running)?))?;
            let next = DelegateBinding::parse(&json::canonical(&next)?)?;
            let witness = checkout(deadline)?;
            snapshot.put(binding.task_id(), running)?;
            let raw = json::pretty(&snapshot.plan)?;
            let old = snapshot
                .raw
                .take()
                .ok_or_else(|| error("canonical Plan missing"))?;
            let mut admitted = false;
            let result = plan.plan_storage().mutate_checked(
                &plan.context.plan_file,
                Some(&old),
                &raw,
                MAX_JSON,
                false,
                Some(|| {
                    Context::before(plan.interrupted, Some(deadline))?;
                    snapshot.markers = None;
                    require(
                        !plan.expired_at(&mut snapshot, &task, true, &delegation_time()?)?,
                        "Plan delegation lease expired before Start",
                    )?;
                    guard.check().map_err(delegate_lock_error)?;
                    recheck_checkout(&witness)?;
                    Context::before(plan.interrupted, Some(deadline))?;
                    admitted = true;
                    Ok(())
                }),
            );
            if !admitted && plan.resolve_delegate_group(&groups, deadline)? {
                continue;
            }
            result?;
            return Ok(next);
        }
    }

    fn delegate_task(&self, s: &mut Snapshot, binding: &DelegateBinding) -> Result<Value> {
        require(
            self.context.canonical(),
            "delegation requires the canonical Plan",
        )?;
        s.require_contract()?;
        let task = s.task(binding.task_id())?.clone();
        require(
            s.plan.get("plan_id") == binding.0.get("plan_id")
                && ["claimed", "running"].contains(&task.text("state"))
                && task.get("lease_id") == binding.0.get("lease_id")
                && task.get("lease_epoch") == binding.0.get("lease_epoch")
                && json::hash(&task)? == binding.0.text("task_sha256")
                && self.git().text(&["rev-parse", "HEAD"])? == binding.base_sha(),
            "Plan delegation task, lease, scope or base changed",
        )?;
        require(
            crate::contracts::stale(&s.registry, &task)?.is_empty()
                && !self.expired_at(s, &task, true, &delegation_time()?)?,
            "Plan delegation contract is stale or its lease expired",
        )?;
        Ok(task)
    }

    pub fn execute(&self, req: &Request) -> Result<Response> {
        self.execute_inner(req, None, None)
    }

    fn capture_delegate_result(
        &self,
        req: &Request,
        binding: &DelegateBinding,
        snapshot: &Snapshot,
        index: &Value,
    ) -> Result<(Request, Value)> {
        let task = snapshot.task(binding.task_id())?;
        let issued = DelegateBinding::parse(&json::canonical(index.get("plan_binding"))?)?;
        require(
            index.get("schema").is_int(1)
                && index.text("kind") == "delegate"
                && receipt_id(index.text("event_id"), 160)
                && receipt_id(index.text("attempt_id"), 160)
                && receipt_id(index.text("operation_id"), 160)
                && index.text("task_id") == binding.task_id()
                && index.text("provider") == task.text("provider")
                && index.text("base_sha") == binding.base_sha()
                && index.get("exit").is_int(0)
                && index.get("verify_exit").is_int(0)
                && index.text("verification") == "passed"
                && index.text("state") == "review"
                && index.get("no_change") == &Value::Bool(false)
                && ["plan_id", "task_id", "lease_id", "lease_epoch", "base_sha"]
                    .iter()
                    .all(|key| issued.0.get(key) == binding.0.get(key)),
            "native delegation index does not bind this verified task result",
        )?;
        let rows = self.source.rows(".oms/artifacts/index.jsonl", false)?;
        let matching = rows
            .iter()
            .filter(|row| row.get("event_id") == index.get("event_id"))
            .collect::<Vec<_>>();
        require(
            matching.len() == 1 && matching[0] == index,
            "native delegation index changed before capture",
        )?;
        let mut request = req.clone();
        let mut retained = object([
            ("schema", number(1)),
            ("kind", string("native-delegate-review")),
            ("plan_id", binding.0.get("plan_id").clone()),
            ("lease_id", string(binding.lease_id())),
            ("index", crate::decision::retain_native(&self.store, index)?),
        ]);
        for (field, digest, suffix) in [
            ("artifact", "artifact_sha256", ".md"),
            ("patch", "patch_sha256", ".patch"),
        ] {
            let path = index.get(field).str()?;
            require(
                !Path::new(path).is_absolute()
                    && !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                    && json::hex(index.text(digest), 64),
                "native delegation evidence requires a bounded repository path and digest",
            )?;
            let absolute = self.context.execution_repo.join(path);
            let supplied = Path::new(req.value(field)?);
            let supplied = if supplied.is_absolute() {
                supplied.to_owned()
            } else {
                self.context.execution_repo.join(supplied)
            };
            require(
                supplied == absolute,
                "Plan Review path differs from the native delegation index",
            )?;
            let bytes = self.source.read(path, MAX_PATCH)?;
            require(
                !bytes.is_empty() && json::sha(&bytes) == index.text(digest),
                "native delegation result bytes changed before capture",
            )?;
            let (reference, _) =
                self.store
                    .immutable(".oms/plan/delegate-results", &bytes, suffix, MAX_PATCH)?;
            request = request.with(
                field,
                self.context
                    .state_repo
                    .join(reference.text("path"))
                    .as_os_str(),
            );
            retained.set(field, reference)?;
        }
        Ok((request, retained))
    }

    fn delegate_review_evidence(
        &self,
        task: &Value,
        expected_patch: &str,
        base: Option<&str>,
    ) -> Result<Option<Value>> {
        let Some(result) = task.opt("delegate_result") else {
            return Ok(None);
        };
        json::keys(
            result,
            &[
                "schema", "kind", "plan_id", "lease_id", "index", "artifact", "patch",
            ],
            &[],
        )?;
        require(
            result.get("schema").is_int(1)
                && result.text("kind") == "native-delegate-review"
                && result.text("lease_id") == task.text("review_lease_id"),
            "invalid native delegate review provenance",
        )?;
        let index_ref = result.get("index");
        let index_raw = self.store.read(index_ref.text("path"), 64 * 1024)?;
        require(
            json::sha(&index_raw) == index_ref.text("sha256"),
            "retained native delegate index changed",
        )?;
        let index = json::parse(&index_raw)?;
        require(
            index.text("task_id") == task.text("id")
                && index.text("provider") == task.text("provider")
                && base.is_none_or(|base| index.text("base_sha") == base)
                && index.get("plan_binding").get("plan_id") == result.get("plan_id")
                && index.get("plan_binding").get("lease_id") == result.get("lease_id"),
            "retained native delegate index changed identity",
        )?;
        for (field, digest) in [("artifact", "artifact_sha256"), ("patch", "patch_sha256")] {
            let reference = result.get(field);
            let bytes = self.store.read(reference.text("path"), MAX_PATCH)?;
            require(
                json::sha(&bytes) == reference.text("sha256")
                    && reference.get("sha256") == index.get(digest)
                    && Path::new(task.text(field))
                        == self.context.state_repo.join(reference.text("path")),
                "retained native delegate review bytes or paths changed",
            )?;
        }
        require(
            result.get("patch").text("sha256") == expected_patch,
            "landing patch is not the exact native delegated result",
        )?;
        Ok(Some(index))
    }

    /// Select only the retained, verified Review from this exact provider call.
    pub fn check_delegate_review(
        &self,
        issued: &DelegateBinding,
        operation: &str,
    ) -> Result<Value> {
        self.context.parent()?;
        let mut snapshot = self.load_from_snapshot(self.read_locked_snapshot()?)?;
        snapshot.require_contract()?;
        let task = snapshot.task(issued.task_id())?.clone();
        require(
            snapshot.plan.get("plan_id") == issued.0.get("plan_id")
                && task.text("state") == "review"
                && task.text("lease_id") == issued.lease_id()
                && task.text("review_lease_id") == issued.lease_id()
                && task.get("lease_epoch") == issued.0.get("lease_epoch")
                && self.git().text(&["rev-parse", "HEAD"])? == issued.base_sha()
                && crate::contracts::stale(&snapshot.registry, &task)?.is_empty(),
            "issued delegation no longer owns the exact reviewed task",
        )?;
        let patch = task.get("delegate_result").get("patch").text("sha256");
        let index = self
            .delegate_review_evidence(&task, patch, Some(issued.base_sha()))?
            .ok_or_else(|| error("issued delegation lacks retained native review evidence"))?;
        require(
            index.text("operation_id") == operation && index.get("plan_binding") == &issued.0,
            "retained native review belongs to a different delegation operation",
        )?;
        let mut view = self.view(&mut snapshot, &task, true)?;
        view.set("plan_id", snapshot.plan.get("plan_id").clone())?;
        Ok(view)
    }

    fn execute_inner(
        &self,
        req: &Request,
        binding: Option<&DelegateBinding>,
        index: Option<&Value>,
    ) -> Result<Response> {
        use Action::*;
        req.validate()?;
        {
            use crate::cli::{physical_dir, physical_leaf, worktree_root};
            let cwd = &self.context.cwd;
            require(
                req.repo.as_ref().is_none_or(|r| {
                    worktree_root(&physical_dir(cwd, r.as_os_str()), self.interrupted)
                        .is_ok_and(|p| p == self.context.execution_repo)
                }),
                "request repository differs from context",
            )?;
            if let Some(file) = &req.file {
                require(
                    physical_leaf(cwd, file.as_os_str()) == self.context.plan_file,
                    "request plan file differs from context",
                )?;
            }
        }
        if matches!(req.action, TeamRegister | TeamShow | TeamList | TeamCancel) {
            if req.read_only() {
                return crate::team::execute(self, req);
            }
            self.context.parent()?;
            require(
                self.context.canonical(),
                "team authority requires the canonical plan",
            )?;
            require(
                room_id(&self.context.room_id),
                "team mutation requires an enrolled room",
            )?;
            let response = self.locked(false, Some(&self.context.room_id), || {
                crate::team::execute(self, req)
            })?;
            if req.action != TeamCancel {
                return Ok(response);
            }
            let retirement = match self.services.team_withdrawn(
                &self.context,
                req.value("id")?,
                req.value("expected-team-sha256")?,
            ) {
                Ok(value) => value,
                Err(error) => Some(object([
                    ("status", string("unknown")),
                    ("reason", string(error.message)),
                ])),
            };
            if let Some(retirement) = retirement {
                let mut result = json::parse(&response.stdout)?;
                let mut record = result.get(req.value("id")?).clone();
                record.set("resource_retirement", retirement)?;
                result.set(req.value("id")?, record)?;
                return Response::json(&result);
            }
            return Ok(response);
        }
        if matches!(
            req.action,
            ContractPropose | ContractAccept | ContractList | ContractShow
        ) {
            return self.locked(false, None, || crate::contracts::execute(self, req));
        }
        if matches!(
            req.action,
            RefreshContracts
                | Init
                | Retire
                | ApplyProposal
                | Add
                | AddBatch
                | Cancel
                | RelocateDecision
                | Amend
                | Link
                | EnsureLineage
                | RecoverOwner
                | Finish
                | Land
                | SnapshotDecision
        ) || req.action.completion()
        {
            self.context.parent()?;
        }
        for name in ["goal", "accept"] {
            screen(req.text(name)?.as_bytes())?;
        }
        if matches!(
            req.action,
            Cancel | RelocateDecision | Amend | Link | Retire
        ) {
            screen(req.text("reason")?.as_bytes())?;
        }
        if req.action == SnapshotDecision {
            return self.locked(false, None, || {
                Response::json_line(
                    &crate::decision::snapshot(
                        &self.store,
                        req.value("decision-artifact")?,
                        req.value("expected-decision-artifact-sha256")?,
                    )?,
                    true,
                    false,
                )
            });
        }
        if req.action == LintVerify {
            let hits = crate::floor::incompatible_reads(
                req.value("verify")?,
                &split_list(req.value("allowed")?)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            );
            return Ok(if hits.is_empty() {
                Response::text("lint-verify: ok (no content reads of allowed paths)")
            } else {
                Response {
                    code: 2,
                    stdout: hits
                        .iter()
                        .map(|(p, r)| format!("floor_incompatible_verifier: {p} ({r})\n"))
                        .collect::<String>()
                        .into_bytes(),
                    stderr: vec![],
                }
            });
        }
        if req.action == Retire {
            return crate::retirement::execute(self, req);
        }
        if req.action.completion() {
            return crate::completion::execute(self, req);
        }
        if req.action == Accept {
            return crate::acceptance::execute(self, req, None);
        }
        if req.read_only() {
            return self.query(req, &mut self.load()?);
        }
        self.locked(
            matches!(
                req.action,
                RecoverLease | RecoverOwner | Cancel | RelocateDecision | Amend | Link
            ),
            None,
            || {
                if self.context.canonical() && !(req.action == RecoverLease && req.has("check")) {
                    crate::retirement::cleanup(self)?;
                }
                let mut s = self.load()?;
                if let Some(binding) = binding
                    && let Err(error) = self.delegate_task(&mut s, binding)
                {
                    if req.action == Review {
                        self.store.append_private_atomic(
                            ".oms/plan/late-results.jsonl",
                            &object([
                                ("schema", number(1)),
                                ("ts", string(&self.context.ts)),
                                ("task_id", string(binding.task_id())),
                                ("supplied_lease_id", string(binding.lease_id())),
                                ("reason", string("stale_delegate_binding")),
                                ("artifact", string(req.text("artifact")?)),
                                ("patch", string(req.text("patch")?)),
                            ]),
                        )?;
                    }
                    return Err(error);
                }
                let captured = if let (Some(binding), Some(index)) = (binding, index) {
                    let captured = self.capture_delegate_result(req, binding, &s, index)?;
                    // File capture and Plan lock acquisition may consume the
                    // lease. Neither extends the worker's issued authority.
                    self.delegate_task(&mut s, binding)?;
                    Some(captured)
                } else {
                    None
                };
                let selected = captured.as_ref().map_or(req, |(request, _)| request);
                let response =
                    self.mutate(selected, &mut s, captured.as_ref().map(|(_, proof)| proof))?;
                if let Some(binding) = binding {
                    let mut next = binding.0.clone();
                    next.set(
                        "task_sha256",
                        string(json::hash(s.task(binding.task_id())?)?),
                    )?;
                    Response::json(&next)
                } else {
                    Ok(response)
                }
            },
        )
    }
    fn contract_verdict(&self, plan: &Value) -> Result<Value> {
        let project = project_snapshot(&self.source)?;
        let contract = plan.get("project_contract");
        let bound = !matches!(contract, Value::Null);
        let valid = contract.get("schema").is_int(1) && json::hex(contract.text("spec_sha256"), 64);
        let current = if project.get("healthy") == &Value::Bool(true) {
            project.text("sha256")
        } else {
            ""
        };
        let blocker = if !bound {
            ""
        } else if !valid {
            "invalid-contract"
        } else if project.get("healthy") != &Value::Bool(true) {
            "project-unreadable"
        } else {
            match project.text("state") {
                "missing" => "project-missing",
                "draft" => "project-draft",
                "confirmed" | "legacy-active" => {
                    if current == contract.text("spec_sha256") {
                        ""
                    } else {
                        "project-drift"
                    }
                }
                _ => "project-invalid",
            }
        };
        Ok(object([
            ("bound", Value::Bool(bound)),
            ("satisfied", Value::Bool(blocker.is_empty())),
            ("project_state", project.get("state").clone()),
            ("project_present", project.get("present").clone()),
            ("project_healthy", project.get("healthy").clone()),
            ("blocker", string(blocker)),
            (
                "expected_spec_sha256",
                string(if valid {
                    contract.text("spec_sha256")
                } else {
                    ""
                }),
            ),
            ("current_spec_sha256", string(current)),
        ]))
    }
    pub(crate) fn markers<'s>(&self, s: &'s mut Snapshot) -> Result<&'s [Value]> {
        if s.markers.is_none() {
            s.markers = Some(self.worker_markers()?);
        }
        Ok(s.markers.as_deref().unwrap())
    }
    pub(crate) fn worker_markers(&self) -> Result<Vec<Value>> {
        let names = self.store.names(".oms/delegations", 4096, true)?;
        let mut rows = Vec::new();
        for name in names {
            if name.as_encoded_bytes().ends_with(b".json") {
                let raw = self
                    .store
                    .read(Path::new(".oms/delegations").join(name), 64 * 1024)?;
                let marker = json::parse(&raw)?;
                marker.object()?;
                rows.push(marker);
            }
        }
        Ok(rows)
    }
    pub(crate) fn live_veto(
        &self,
        s: &mut Snapshot,
        selected: &[&str],
        noun: &str,
        all: bool,
    ) -> Result<()> {
        for row in self.markers(s)? {
            require(
                marker_typed(row),
                format!("worker marker set is malformed or unproven; {noun} refused"),
            )?;
            if (selected.contains(&row.text("task_id"))
                || all && row.text("task_id").is_empty() && row.text("access") != "read")
                && marker_alive(row)
            {
                return Err(error(format!("live worker marker prevents {noun}")));
            }
        }
        Ok(())
    }
    pub(crate) fn expired(&self, s: &mut Snapshot, t: &Value, running: bool) -> Result<bool> {
        self.expired_at(s, t, running, &self.context.ts)
    }
    fn expired_at(&self, s: &mut Snapshot, t: &Value, running: bool, now: &str) -> Result<bool> {
        if !(t.text("state") == "claimed" || running && t.text("state") == "running") {
            return Ok(false);
        }
        let Some(age) = age(t, now) else {
            return Ok(false);
        };
        if age < i128::from(task_ttl(t, self.context.claim_ttl)) {
            return Ok(false);
        }
        Ok(!self.markers(s)?.iter().any(|m| {
            m.text("task_id") == t.text("id")
                && m.text("lease_id") == t.text("lease_id")
                && (!marker_typed(m) || marker_alive(m))
        }))
    }
    /// Fence a native panel application against the same lease expiry used by
    /// the plan's review mutation, including running tasks and live markers.
    pub fn verify_panel_review_lease(&self, expected: &Value) -> Result<()> {
        let id = expected.text("id");
        require(
            self.context.canonical()
                && valid_id(id)
                && lineage(expected.text("plan_id"), "plan_")
                && lineage(expected.text("lease_id"), "lease_")
                && !expected.text("claimed_by_participant").is_empty(),
            "invalid canonical panel acceptance lease",
        )?;
        let mut snapshot = self.load()?;
        let task = snapshot.task(id)?.clone();
        require(
            snapshot.plan.get("plan_id") == expected.get("plan_id")
                && ["claimed", "running"].contains(&task.text("state"))
                && [
                    "id",
                    "state",
                    "lease_id",
                    "claimed_by_participant",
                    "provider",
                    "verify",
                    "claimed_at",
                    "updated",
                    "ttl",
                ]
                .iter()
                .all(|&field| task.get(field) == expected.get(field)),
            "canonical panel acceptance task or lease changed",
        )?;
        require(
            !self.expired(&mut snapshot, &task, true)?,
            "canonical panel acceptance task lease expired",
        )
    }
    pub(crate) fn actionable(&self, s: &mut Snapshot, t: &Value) -> Result<bool> {
        if !crate::contracts::stale(&s.registry, t)?.is_empty()
            || t.get("executor_id").truth()
            || t.get("executor_soul_sha256").truth()
            || s.contract.get("satisfied") != &Value::Bool(true)
            || !s.deps_done(t)
        {
            return Ok(false);
        }
        Ok(t.text("state") == "ready" || self.expired(s, t, false)?)
    }
    fn start_task(&self, req: &Request, task: &mut Value) -> Result<()> {
        require(
            !task.get("executor_id").truth() && !task.get("executor_soul_sha256").truth(),
            "Soul executor receipt is retired; preserve old evidence and create a fresh plan task",
        )?;
        let id = task.text("id");
        let state = task.text("state");
        require(
            state == "claimed",
            format!("task {id} is {state}; claim it first"),
        )?;
        self.lease(req, task)?;
        task.set("state", string("running"))
    }

    fn mutate(
        &self,
        req: &Request,
        s: &mut Snapshot,
        delegate_result: Option<&Value>,
    ) -> Result<Response> {
        use Action::*;
        match req.action {
            Init => {
                let archive = if self.context.canonical() && s.raw.is_some() {
                    crate::retirement::archive_replacement(self, s)?
                } else {
                    String::new()
                };
                s.plan = object([
                    ("schema", number(3)),
                    (
                        "plan_id",
                        string(format!("plan_{}", storage::random_hex(16)?)),
                    ),
                    ("goal", string(req.text("goal")?)),
                    ("accept", string(req.text("accept")?)),
                    ("tasks", empty()),
                ]);
                self.save(s)?;
                let mut out = b"plan: initialized (".to_vec();
                out.extend_from_slice(self.context.plan_file.as_os_str().as_encoded_bytes());
                out.extend_from_slice(b")\n");
                if !archive.is_empty() {
                    out.extend(format!("plan: archived replaced plan ({archive})\n").as_bytes());
                }
                return Ok(Response {
                    stdout: out,
                    ..Default::default()
                });
            }
            EnsureLineage => {
                require(
                    s.raw.is_some(),
                    "no plan; initialize it before ensuring lineage",
                )?;
                let before = s.plan.opt("plan_id").is_some();
                let id = ensure_lineage(&mut s.plan)?;
                if !before {
                    self.save(s)?;
                }
                return Ok(Response::text(format!(
                    "plan: lineage {} ({id})",
                    if before {
                        "already initialized"
                    } else {
                        "initialized"
                    }
                )));
            }
            RefreshContracts => return crate::contracts::refresh(self, req, s),
            Add => return self.add(req, s),
            AddBatch => return self.add_batch(req, s),
            ApplyProposal => return self.proposal(req, s),
            Amend => return self.amend(req, s),
            Link => return self.link(req, s),
            Cancel => return self.cancel(req, s),
            RelocateDecision => return self.relocate(req, s),
            RecoverOwner => return self.recover_owner(req, s),
            Reclaim => return self.reclaim(req, s),
            Next => return self.next(req, s),
            _ => {}
        }
        let id = req.value("id")?;
        require(valid_id(id), "--id must match [A-Za-z0-9._-]+")?;
        let mut t = s.task(id)?.clone();
        let state = t.text("state").to_owned();
        if matches!(req.action, Claim | Release | Repair | Block) && req.has("expected-task-sha256")
        {
            let expected = req.value("expected-task-sha256")?;
            require(
                json::hex(expected, 64)
                    && json::hash(&t)? == expected
                    && (req.action != Release || state == "claimed"),
                "task changed before the conditional Plan transition",
            )?;
        }
        if matches!(req.action, Claim | Land | Finish)
            || req.action == Repair && req.has("expected-review-binding")
        {
            let stale = crate::contracts::stale(&s.registry, &t)?;
            require(
                stale.is_empty(),
                format!("contract_stale: {}", stale.join(", ")),
            )?;
        }
        if req.action == Review && req.has("lease-id") {
            let supplied = req.value("lease-id")?;
            if !["claimed", "running"].contains(&state.as_str())
                || t.text("lease_id") != supplied
                || self.expired(s, &t, true)?
            {
                let row = object([
                    ("schema", number(1)),
                    ("ts", string(&self.context.ts)),
                    ("task_id", string(id)),
                    ("supplied_lease_id", string(supplied)),
                    ("current_lease_id", string(t.text("lease_id"))),
                    ("current_state", string(&state)),
                    ("reason", string("stale_lease")),
                    ("artifact", string(req.text("artifact")?)),
                    ("patch", string(req.text("patch")?)),
                    ("result_reason", string(req.text("reason")?)),
                ]);
                self.store
                    .append_private_atomic(".oms/plan/late-results.jsonl", &row)?;
                return Err(drift(
                    "stale lease; late result preserved in .oms/plan/late-results.jsonl",
                ));
            }
        }
        if matches!(req.action, Claim | Review | Repair | Land | Finish) {
            require(
                !t.get("executor_id").truth() && !t.get("executor_soul_sha256").truth(),
                "Soul executor receipt is retired; preserve old evidence and create a fresh plan task",
            )?;
        }
        if matches!(req.action, Review | Finish) {
            let patch = if req.text("patch")?.is_empty() {
                t.text("patch")
            } else {
                req.text("patch")?
            };
            self.check_managed_patch(patch)?;
        }
        match req.action {
            Claim => {
                s.require_contract()?;
                require(
                    state == "ready",
                    format!(
                        "task {id} is {state}; only a ready task can be claimed (reopen blocked first)"
                    ),
                )?;
                require(
                    s.deps_done(&t),
                    format!("task {id} has unfinished dependencies"),
                )?;
                self.claim(req, &mut t, true)?;
            }
            Start => self.start_task(req, &mut t)?,
            Touch => {
                require(
                    ["claimed", "running"].contains(&state.as_str()),
                    format!("task {id} is {state}; only a claimed/running task can be touched"),
                )?;
                self.lease(req, &t)?;
                t.set("claimed_at", string(&self.context.ts))?;
            }
            Review => {
                require(
                    ["claimed", "running"].contains(&state.as_str()),
                    format!("task {id} is {state}; only a claimed/running task can go to review"),
                )?;
                self.lease(req, &t)?;
                require(
                    req.text("executor-id")?.is_empty()
                        && req.text("executor-soul-sha256")?.is_empty(),
                    "Soul executor receipts cannot be created; use task brief and lease",
                )?;
                let acceptance = std::env::var("OMS_PANEL_ACCEPT_RECORD_JSON")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|s| json::parse_legacy(s.as_bytes(), MAX_JSON))
                    .transpose()?;
                if let Some(record) = &acceptance {
                    self.panel_accept_record(id, &t, &s.plan, record, req, false)?;
                }
                let reviewed = self.capture_review_evidence(req, &t)?;
                t.set("state", string("review"))?;
                evidence(req, &mut t)?;
                t.set("review_lease_id", t.get("lease_id").clone())?;
                t.set("executor_id", string(""))?;
                t.set("executor_soul_sha256", string(""))?;
                t.set("repair_artifact", string(""))?;
                if let Some(result) = delegate_result {
                    t.set("delegate_result", result.clone())?;
                } else {
                    t.remove("delegate_result");
                }
                if let Some(record) = acceptance {
                    t.set("panel_accept", record)?;
                } else {
                    t.remove("panel_accept");
                }
                if let Some(record) = reviewed {
                    t.set("review_evidence", record)?;
                } else {
                    t.remove("review_evidence");
                }
            }
            Repair => {
                if req.has("expected-review-binding") {
                    let proof =
                        ReviewFence::parse(req.value("expected-review-binding")?.as_bytes())?;
                    self.require_landing_review(&s.plan, &t, &proof)?;
                    self.delegate_review_evidence(
                        &t,
                        proof.patch_sha256(),
                        Some(proof.base_sha()),
                    )?;
                }
                require(
                    state == "review",
                    format!("task {id} is {state}; only reviewed work can enter repair"),
                )?;
                req.value("lease-id")?;
                self.lease(req, &t)?;
                require(
                    !t.text("lease_id").is_empty()
                        && t.text("review_lease_id") == t.text("lease_id"),
                    "review patch lease mismatch; repair is stale",
                )?;
                require(
                    !t.text("artifact").is_empty() && !t.text("patch").is_empty(),
                    "review is missing artifact/patch evidence",
                )?;
                require(
                    t.get("repair_count").int().is_ok_and(|n| n == 0),
                    "bounded review repair was already used or counter is invalid",
                )?;
                t.remove("panel_accept");
                t.remove("review_evidence");
                t.set("state", string("claimed"))?;
                t.set("claimed_at", string(&self.context.ts))?;
                t.set("reason", string(""))?;
                t.set("repair_count", number(1))?;
                if !req.text("artifact")?.is_empty() {
                    t.set("repair_artifact", string(req.text("artifact")?))?;
                }
            }
            Land => self.land(req, &mut t, &s.plan)?,
            Finish => {
                if self.finish(req, s, &mut t)? {
                    return Ok(Response::default());
                }
            }
            Block => {
                let reason = req.value("reason")?;
                require(
                    state != "cancelled",
                    format!("task {id} is cancelled; it cannot be blocked"),
                )?;
                if ["claimed", "running", "review", "landing"].contains(&state.as_str()) {
                    self.lease(req, &t)?;
                }
                if t.get("completion_kind").truth() {
                    self.context.parent()?;
                    let kind = t.remove("completion_kind").unwrap();
                    let receipt = t
                        .remove("completion_receipt")
                        .ok_or_else(|| error("missing completion receipt"))?;
                    history(
                        &mut t,
                        object([
                            ("schema", number(1)),
                            ("kind", string("completion-reactivated")),
                            ("ts", string(&self.context.ts)),
                            ("completion_kind", kind),
                            ("completion_receipt", receipt),
                            ("reason", string(reason)),
                        ]),
                    )?;
                }
                t.set("state", string("blocked"))?;
                t.set("reason", string(reason))?;
            }
            Release => {
                require(
                    ["claimed", "running", "review", "landing"].contains(&state.as_str()),
                    format!(
                        "task {id} is {state}; only a claimed/running/review/landing task can be released"
                    ),
                )?;
                self.lease(req, &t)?;
                if req.has("expected-landing-receipt-sha256")
                    || t.opt(LANDING_REVIEW_SHA256).is_some()
                {
                    self.context.parent()?;
                    self.require_landing_transition(req, &s.plan, &t)?;
                }
                clear_claim(s, &mut t)?;
            }
            Reopen => {
                require(
                    state == "blocked",
                    format!("task {id} is {state}; only a blocked task can be reopened"),
                )?;
                clear_claim(s, &mut t)?;
            }
            RecoverLease => {
                let expected = req.value("expected-state")?;
                require(
                    STATES.contains(&expected),
                    "recover-lease requires a valid --expected-state",
                )?;
                let supplied = req.value("lease-id")?;
                if t.text("lease_id") != supplied
                    || state != expected
                    || !["claimed", "running"].contains(&expected)
                {
                    return Err(drift(format!(
                        "plan: task {id} no longer holds that exact claimed/running state+lease"
                    )));
                }
                self.markers_directory(req)?;
                let matching = self
                    .markers(s)?
                    .iter()
                    .filter(|m| m.text("task_id") == id && m.text("lease_id") == supplied)
                    .collect::<Vec<_>>();
                if matching.iter().any(|m| !marker_typed(m)) {
                    return Ok(Response{code:3,stdout:vec![],stderr:format!("plan-recovery-outcome: unproven\nplan: task {id} has an unproven exact worker marker\n").into_bytes()});
                }
                if matching.iter().any(|m| marker_alive(m)) {
                    return Ok(Response{code:3,stdout:vec![],stderr:format!("plan-recovery-outcome: veto\nplan: task {id} still has a live exact worker marker\n").into_bytes()});
                }
                if req.has("check") {
                    return Ok(Response::text(format!(
                        "plan: task {id} exact state+lease is recoverable"
                    )));
                }
                clear_claim(s, &mut t)?;
            }
            _ => return Err(error(format!("unhandled action: {}", req.action.name()))),
        }
        t.set("updated", string(&self.context.ts))?;
        let after = t.text("state").to_owned();
        s.put(id, t)?;
        self.save(s)?;
        if req.action == Claim && req.has("json") {
            let task = s.task(id)?.clone();
            let mut issued = self.view(s, &task, true)?;
            issued.set("plan_id", s.plan.get("plan_id").clone())?;
            return Response::json(&issued);
        }
        Ok(Response::text(format!("plan: {id} -> {after}")))
    }
    pub(crate) fn lease(&self, req: &Request, t: &Value) -> Result<()> {
        let supplied = if req.has("lease-id") {
            req.text("lease-id")?.to_owned()
        } else {
            std::env::var("OMS_PLAN_LEASE_ID").unwrap_or_default()
        };
        require(
            supplied.is_empty() || supplied == t.text("lease_id"),
            format!("task {} lease mismatch; worker is stale", t.text("id")),
        )?;
        require(
            !self.context.child || t.text("lease_id").is_empty() || !supplied.is_empty(),
            format!(
                "task {} requires --lease-id for harness child mutation",
                t.text("id")
            ),
        )
    }
    pub(crate) fn claim(&self, req: &Request, t: &mut Value, clear_repair: bool) -> Result<()> {
        let provider = t
            .get("assignment")
            .opt("provider")
            .map(Value::str)
            .transpose()?
            .unwrap_or(req.text("provider")?);
        require(!provider.is_empty(), "--provider is required for claim")?;
        let provider = self.services.normalize_provider(provider)?;
        t.set("lease_epoch", json::increment(t.get("lease_epoch"))?)?;
        t.set(
            "lease_id",
            string(format!("lease_{}", storage::random_hex(16)?)),
        )?;
        let owner = if req.has("owner-id") {
            req.text("owner-id")?
        } else {
            &self.context.owner_id
        };
        require(
            owner.is_empty() || lineage(owner, "owner_"),
            "autopilot owner id is invalid",
        )?;
        for (name, value) in [
            ("state", "claimed"),
            ("provider", provider.as_str()),
            ("ttl", req.text("ttl")?),
            ("claimed_at", self.context.ts.as_str()),
            ("reason", ""),
            ("autopilot_owner_id", owner),
        ] {
            t.set(name, string(value))?;
        }
        if clear_repair {
            t.set("repair_artifact", string(""))?;
        }
        t.remove("review_evidence");
        if !self.context.panel_main_attempt.is_empty() && receipt_id(&self.context.participant, 80)
        {
            t.set("claimed_by_participant", string(&self.context.participant))?;
        } else {
            t.remove("claimed_by_participant");
        }
        Ok(())
    }
    fn add(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        require(
            matches!(s.plan.get("project_contract"), Value::Null),
            "contract-bound plans accept new tasks only through a reviewed proposal",
        )?;
        let route = if req.has("assignment") {
            json::parse_legacy(req.text("assignment")?.as_bytes(), MAX_JSON)?
        } else {
            empty()
        };
        assignment(&route)?;
        let found = req.text("discovered-from")?;
        if !found.is_empty() {
            require(
                valid_id(found),
                "--discovered-from must match [A-Za-z0-9._-]+",
            )?;
            s.task(found)?;
        }
        let issues = Issues::load(self);
        let issue = if req.has("issue") {
            Some(issues.canonical(self, req.value("issue")?)?)
        } else if issues.required()? {
            // Local decomposition of shared work keeps its parent's issue.
            let inherited = s.task(found).map(|t| t.text("issue")).unwrap_or("");
            require(
                !inherited.is_empty(),
                "GitHub project mode: add requires --issue, or --discovered-from a task bound to an issue",
            )?;
            Some(issues.canonical(self, inherited)?)
        } else {
            None
        };
        let id = if req.text("id")?.is_empty() && !found.is_empty() {
            loop {
                let candidate = format!("d-{}", &storage::random_hex(4)?[..6]);
                if s.task(&candidate).is_err() {
                    break candidate;
                }
            }
        } else {
            req.value("id")?.to_owned()
        };
        require(valid_id(&id), "--id must match [A-Za-z0-9._-]+")?;
        require(s.task(&id).is_err(), format!("task already exists: {id}"))?;
        let title = req.value("title")?;
        plain(title, "task title")?;
        plain(req.text("verify")?, "task verify")?;
        plain(req.text("role")?, "task role")?;
        let depends = split_list(req.text("depends")?);
        let mut task = new_task(
            &id,
            title,
            depends,
            split_list(req.text("allowed")?),
            split_list(req.text("forbidden")?),
            req.text("verify")?,
            req.text("role")?,
            route,
            &self.context.ts,
        )?;
        if req.has("resources") || req.has("contracts") || req.has("priority") {
            let csv = |value: &str| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            scheduling_metadata(
                &mut task,
                &csv(req.text("resources")?),
                &csv(req.text("contracts")?),
                if req.has("priority") {
                    req.value("priority")?
                        .parse::<i32>()
                        .map_err(|_| error("--priority must be an i32"))?
                } else {
                    0
                },
            )?;
        }
        if !found.is_empty() {
            task.set("discovered_from", string(found))?;
        }
        if let Some(issue) = issue {
            task.set("issue", string(issue))?;
        }
        let mut graph = s.ordered()?;
        graph.push(task.clone());
        validate_graph(&graph)?;
        s.put(&id, task)?;
        self.save(s)?;
        let mut response = Response::text(format!("plan: added {id} ({title})"));
        for warning in self
            .services
            .verify_selector_warnings(&self.context.execution_repo, req.text("verify")?)?
        {
            response.stderr.extend(
                format!(
                    "warning: unknown verify selector: {warning} (fine if this task creates it)\n"
                )
                .as_bytes(),
            );
        }
        Ok(response)
    }
    fn markers_directory(&self, req: &Request) -> Result<()> {
        if let Some(path) = req.path("markers-dir") {
            let expected = self.context.state_repo.join(".oms/delegations");
            let supplied = crate::cli::physical_leaf(&self.context.cwd, path.as_os_str());
            require(
                supplied == expected,
                "worker marker directory must be repo-local .oms/delegations directory",
            )?;
        }
        Ok(())
    }
    pub(crate) fn canonical_existing(&self, s: &Snapshot) -> Result<()> {
        require(
            self.context.canonical() && s.raw.is_some(),
            "operation requires an existing regular canonical repo-local plan",
        )
    }
    pub(crate) fn exact_cas(
        &self,
        req: &Request,
        s: &Snapshot,
        t: &Value,
        noun: &str,
    ) -> Result<()> {
        self.canonical_existing(s)?;
        let plan = req.value("expected-plan-sha256")?;
        let task = req.value("expected-task-sha256")?;
        require(
            json::hex(plan, 64) && json::hex(task, 64),
            format!("{noun} requires exact lowercase plan and task SHA-256 values"),
        )?;
        require(
            s.raw.as_ref().is_some_and(|raw| json::sha(raw) == plan),
            format!("active plan changed after {noun} was reviewed"),
        )?;
        require(
            json::hash(t)? == task,
            format!("task changed after {noun} was reviewed"),
        )
    }
    /// Bind a task to one same-repository issue. Inactive tasks only: every
    /// task digest (team specs, cancellation, completion and landing receipts)
    /// covers the new field.
    fn link(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        self.canonical_existing(s)?;
        let id = req.value("task")?;
        require(valid_id(id), "--task must match [A-Za-z0-9._-]+")?;
        let mut t = s.task(id)?.clone();
        let issues = Issues::load(self);
        let issue = issues.canonical(self, req.value("issue")?)?;
        let current = t.text("issue").to_owned();
        if !current.is_empty()
            && oms_github::IssueRef::parse(&current, issues.origin(self)?)
                .is_ok_and(|r| r.to_string() == issue)
        {
            return Ok(Response::text(format!(
                "plan: {id} already linked to {current}"
            )));
        }
        require(
            matches!(s.plan.get("project_contract"), Value::Null),
            "proposal-bound tasks take their issue from the reviewed proposal",
        )?;
        require(
            ["ready", "blocked"].contains(&t.text("state")) && !t.get("lease_id").truth(),
            "link requires an unleased ready or blocked task",
        )?;
        self.live_veto(s, &[id], "issue link", true)?;
        if !current.is_empty() {
            require(
                req.has("reason"),
                format!("task {id} is linked to {current}; relinking requires --reason"),
            )?;
            let reason = reason(req, "issue relink")?;
            history(
                &mut t,
                object([
                    ("kind", string("issue-relinked")),
                    ("before", string(&current)),
                    ("after", string(&issue)),
                    ("reason", string(reason)),
                    ("ts", string(&self.context.ts)),
                ]),
            )?;
        }
        t.set("issue", string(&issue))?;
        s.put(id, t)?;
        self.save(s)?;
        Ok(Response::text(format!("plan: {id} linked to {issue}")))
    }
    fn amend(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let id = req.value("id")?;
        let mut t = s.task(id)?.clone();
        self.exact_cas(req, s, &t, "amendment")?;
        require(
            matches!(s.plan.get("project_contract"), Value::Null),
            "proposal-bound tasks cannot be amended",
        )?;
        require(
            ["ready", "blocked"].contains(&t.text("state")) && !t.get("lease_id").truth(),
            "amend requires an unleased ready or blocked task",
        )?;
        self.live_veto(s, &[id], "amendment", true)?;
        let reason = reason(req, "amendment")?;
        require(
            req.has("allowed") || req.has("verify"),
            "amend requires allowed or verify",
        )?;
        let before = object([
            (
                "allowed_paths",
                t.opt("allowed_paths")
                    .cloned()
                    .unwrap_or(Value::Array(vec![])),
            ),
            ("verify", t.opt("verify").cloned().unwrap_or(string(""))),
        ]);
        let allowed = if req.has("allowed") {
            clean_paths(&split_list(req.text("allowed")?))?
        } else {
            before
                .get("allowed_paths")
                .strings()?
                .into_iter()
                .map(str::to_owned)
                .collect()
        };
        let verify = if req.has("verify") {
            req.text("verify")?
        } else {
            before.text("verify")
        };
        plain(verify, "task verify")?;
        require(
            !(before.get("allowed_paths").truth() && allowed.is_empty())
                && !(!before.text("verify").trim().is_empty() && verify.trim().is_empty()),
            "amend cannot remove a task's path limits or its verify",
        )?;
        lint(verify, &allowed)?;
        let after = object([
            ("allowed_paths", list(allowed.iter().map(String::as_str))),
            ("verify", string(verify)),
        ]);
        t.set("allowed_paths", after.get("allowed_paths").clone())?;
        t.set("verify", after.get("verify").clone())?;
        history(
            &mut t,
            object([
                ("kind", string("amended")),
                ("before", before),
                ("after", after),
                ("reason", string(reason)),
                ("ts", string(&self.context.ts)),
            ]),
        )?;
        let hash = json::hash(&t)?;
        s.put(id, t)?;
        self.save(s)?;
        Ok(Response::text(hash))
    }
    fn cancel(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        self.canonical_existing(s)?;
        require(
            req.value("operator-consent")? == "cancel",
            "cancel requires explicit operator consent",
        )?;
        let reason = reason(req, "cancellation")?;
        let expected = req.value("expected-plan-sha256")?;
        require(
            json::hex(expected, 64) && s.raw.as_ref().is_some_and(|r| json::sha(r) == expected),
            "active plan changed after cancellation was reviewed",
        )?;
        let batch = req.has("all-inactive");
        let mut selected = Vec::new();
        if batch {
            for (_, task) in s.tasks()? {
                require(
                    task.opt("state").is_some(),
                    "plan contains a task without state; cancellation refused",
                )?;
                if ["ready", "blocked"].contains(&task.text("state"))
                    && !task.get("lease_id").truth()
                {
                    selected.push(task.clone());
                }
            }
            require(!selected.is_empty(), "no inactive unleased tasks to cancel")?;
        } else {
            let task = s.task(req.value("id")?)?;
            self.exact_cas(req, s, task, "cancellation")?;
            let state = req.value("expected-state")?;
            let expected_lease = req.value("expected-lease-id")?;
            let lease = if expected_lease == "none" {
                ""
            } else {
                require(
                    receipt_id(expected_lease, 200),
                    "cancel requires --expected-lease-id TOKEN or none",
                )?;
                expected_lease
            };
            require(
                ["ready", "blocked"].contains(&state)
                    && task.text("state") == state
                    && task.text("lease_id") == lease,
                "task state or lease changed after cancellation was reviewed",
            )?;
            selected.push(task.clone());
        }
        require(
            selected.iter().all(|t| t.opt("cancellation").is_none()),
            "task already has cancellation evidence",
        )?;
        self.live_veto(
            s,
            &selected.iter().map(|t| t.text("id")).collect::<Vec<_>>(),
            "cancellation",
            true,
        )?;
        let rel = req.value("decision-artifact")?;
        let proof = crate::decision::snapshot(
            &self.store,
            rel,
            req.value("expected-decision-artifact-sha256")?,
        )?;
        for mut task in selected.clone() {
            let id = task.text("id").to_owned();
            let receipt = object([
                ("schema", number(1)),
                ("kind", string("operator-cancellation")),
                ("ts", string(&self.context.ts)),
                ("operator_consent", string("cancel")),
                ("reason", string(reason)),
                ("state", task.get("state").clone()),
                (
                    "original_updated",
                    task.opt("updated").cloned().unwrap_or(string("")),
                ),
                ("lease_id_sha256", string(json::sha(task.text("lease_id")))),
                ("plan_sha256", string(expected)),
                ("task_sha256", string(json::hash(&task)?)),
                ("decision_artifact", string(rel)),
                ("decision_artifact_sha256", proof.get("sha256").clone()),
            ]);
            task.set("cancellation", receipt)?;
            task.set("state", string("cancelled"))?;
            task.set("updated", string(&self.context.ts))?;
            s.put(&id, task)?;
        }
        self.save(s)?;
        if batch {
            let mut ids = selected.iter().map(|t| t.text("id")).collect::<Vec<_>>();
            ids.sort();
            let mut active = s
                .tasks()?
                .iter()
                .map(|(_, t)| t)
                .filter(|t| ["claimed", "running", "review", "landing"].contains(&t.text("state")))
                .map(|t| t.text("id"))
                .collect::<Vec<_>>();
            active.sort();
            Ok(Response::text(format!(
                "plan: cancelled {}; still active: {}",
                ids.join(", "),
                if active.is_empty() {
                    "none".into()
                } else {
                    active.join(", ")
                }
            )))
        } else {
            Ok(Response::text(format!(
                "plan: {} -> cancelled",
                req.value("id")?
            )))
        }
    }
    fn relocate(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        require(
            req.value("operator-consent")? == "relocate-decision",
            "relocate-decision requires explicit operator consent",
        )?;
        let reason = reason(req, "relocation")?;
        let id = req.value("id")?;
        let mut t = s.task(id)?.clone();
        self.exact_cas(req, s, &t, "relocation")?;
        require(
            t.text("state") == "cancelled",
            "relocation requires a cancelled task",
        )?;
        let mut receipt = t.get("cancellation").clone();
        let bound = receipt.text("decision_artifact_sha256").to_owned();
        let old = receipt.text("decision_artifact").to_owned();
        let new = req.value("decision-artifact")?;
        crate::decision::relative(new)?;
        require(
            new != old,
            "relocation target is the receipt's current decision artifact path",
        )?;
        require(
            req.value("expected-decision-artifact-sha256")? == bound,
            "reviewed decision-artifact digest is not the digest bound in cancellation receipt",
        )?;
        require(
            json::sha(self.store.read(new, 64 * 1024)?) == bound,
            "relocated decision artifact does not match bound digest",
        )?;
        self.live_veto(s, &[id], "relocation", true)?;
        receipt.set("decision_artifact", string(new))?;
        t.set("cancellation", receipt)?;
        s.put(id, t.clone())?;
        let result = json::sha(json::pretty(&s.plan)?);
        let row = object([
            ("schema", number(1)),
            ("kind", string("cancellation-decision-relocation")),
            ("ts", string(&self.context.ts)),
            ("operator_consent", string("relocate-decision")),
            ("reason", string(reason)),
            ("task_id", string(id)),
            ("old_decision_artifact", string(old)),
            ("new_decision_artifact", string(new)),
            ("decision_artifact_sha256", string(bound)),
            (
                "reviewed_plan_sha256",
                string(req.value("expected-plan-sha256")?),
            ),
            (
                "reviewed_task_sha256",
                string(req.value("expected-task-sha256")?),
            ),
            ("resulting_plan_sha256", string(&result)),
            ("resulting_task_sha256", string(json::hash(&t)?)),
        ]);
        self.store
            .append(".oms/plan/cancel-decision-relocations.jsonl", &row)?;
        self.save(s)?;
        Ok(Response::text(format!(
            "plan: relocated {id} decision artifact {} -> {new} (plan sha256 {result})",
            row.text("old_decision_artifact")
        )))
    }
    fn recover_owner(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let owner = if req.has("owner-id") {
            req.value("owner-id")?
        } else {
            &self.context.owner_id
        };
        require(
            lineage(owner, "owner_"),
            "recover-owner requires valid --owner-id",
        )?;
        self.markers_directory(req)?;
        let markers = self.markers(s)?.to_vec();
        let mut recovered = Vec::new();
        let mut preserved = Vec::new();
        for mut t in s.ordered()? {
            if t.text("autopilot_owner_id") != owner {
                continue;
            }
            let id = t.text("id").to_owned();
            let state = t.text("state");
            if ["review", "landing"].contains(&state) {
                preserved.push(id);
                continue;
            }
            if !["claimed", "running"].contains(&state) {
                continue;
            }
            let lease = t.text("lease_id");
            let matching = markers
                .iter()
                .filter(|m| m.text("task_id") == id && m.text("lease_id") == lease)
                .collect::<Vec<_>>();
            let owned = matching
                .iter()
                .filter(|m| m.text("autopilot_owner_id") == owner)
                .count();
            if lease.is_empty()
                || matching.iter().any(|m| !marker_typed(m) || marker_alive(m))
                || (!matching.is_empty() && owned != matching.len())
                || state == "running" && owned == 0
            {
                preserved.push(id);
                continue;
            }
            clear_claim(s, &mut t)?;
            t.set("updated", string(&self.context.ts))?;
            s.put(&id, t)?;
            recovered.push(id);
        }
        if !recovered.is_empty() {
            self.save(s)?;
        }
        if req.has("json") {
            Response::json_line(
                &object([
                    ("owner_id", string(owner)),
                    ("recovered", list(recovered.iter().map(String::as_str))),
                    ("preserved", list(preserved.iter().map(String::as_str))),
                ]),
                true,
                true,
            )
        } else {
            let mut out = format!(
                "plan: recovered {}",
                if recovered.is_empty() {
                    "(none)".into()
                } else {
                    recovered.join(",")
                }
            );
            if !preserved.is_empty() {
                out.push_str(&format!("\nplan: preserved {}", preserved.join(",")));
            }
            Ok(Response::text(out))
        }
    }
    fn reclaim(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
        let supplied = if req.has("ttl") {
            Some(
                req.value("ttl")?
                    .parse::<u64>()
                    .map_err(|_| error("reclaim --ttl must be integer seconds"))?,
            )
        } else {
            None
        };
        let mut count = 0;
        let mut lines = Vec::new();
        for mut t in s.ordered()? {
            let state = t.text("state").to_owned();
            if !(state == "claimed"
                || state == "running" && req.has("include-running")
                || state == "review" && req.has("include-review"))
            {
                continue;
            }
            let Some(age) = age(&t, &self.context.ts) else {
                continue;
            };
            let ttl = if state == "review" {
                supplied.unwrap_or(86400)
            } else {
                task_ttl(&t, supplied.unwrap_or(self.context.claim_ttl))
            };
            if age < i128::from(ttl) {
                continue;
            }
            if state != "review" {
                let veto = self.markers(s)?.iter().any(|m| {
                    m.text("task_id") == t.text("id")
                        && m.text("lease_id") == t.text("lease_id")
                        && (!marker_typed(m) || marker_alive(m))
                });
                if veto {
                    continue;
                }
            }
            let id = t.text("id").to_owned();
            let provider = t.text("provider").to_owned();
            clear_claim(s, &mut t)?;
            t.set("updated", string(&self.context.ts))?;
            s.put(&id, t)?;
            count += 1;
            lines.push(format!(
                "plan: reclaimed {id} from {state} (age {age}s > ttl {ttl}s, was @{})",
                if provider.is_empty() { "?" } else { &provider }
            ));
        }
        if count > 0 {
            self.save(s)?;
        }
        lines.push(format!("plan: reclaimed {count} task(s)"));
        Ok(Response::text(lines.join("\n")))
    }
    /// Freeze the selected review and its evidence before passing it to landing.
    pub fn landing_review_fence(&self, snapshot: &Value, base: &str) -> Result<ReviewFence> {
        let lease = DelegateBinding::parse(&json::canonical(&object([
            ("schema", number(1)),
            ("plan_id", snapshot.get("plan_id").clone()),
            ("task_id", snapshot.get("id").clone()),
            ("task_sha256", snapshot.get("task_sha256").clone()),
            ("lease_id", snapshot.get("lease_id").clone()),
            ("lease_epoch", snapshot.get("lease_epoch").clone()),
            ("base_sha", string(base)),
        ]))?)?;
        self.context.parent()?;
        self.locked(false, None, || {
            let current = self.load()?;
            let task = current.task(lease.task_id())?;
            let report = self.evidence_read(task.text("artifact"), MAX_PATCH)?;
            let patch = self.evidence_read(task.text("patch"), MAX_PATCH)?;
            require(
                !report.is_empty() && !patch.is_empty(),
                "review evidence is empty",
            )?;
            let expected = ReviewFence {
                lease,
                report_sha256: json::sha(&report),
                patch_sha256: json::sha(&patch),
            };
            self.require_landing_review(&current.plan, task, &expected)?;
            self.delegate_review_evidence(task, expected.patch_sha256(), Some(base))?;
            Ok(expected)
        })
    }

    pub fn check_landing_review(&self, expected: &ReviewFence) -> Result<Value> {
        self.context.parent()?;
        self.locked(false, None, || {
            let mut s = self.load()?;
            let task = s.task(expected.task_id())?.clone();
            require(
                crate::contracts::stale(&s.registry, &task)?.is_empty(),
                "Plan landing contracts became stale before admission",
            )?;
            self.require_landing_review(&s.plan, &task, expected)?;
            self.delegate_review_evidence(
                &task,
                expected.patch_sha256(),
                Some(expected.base_sha()),
            )?;
            let mut view = self.view(&mut s, &task, true)?;
            view.set("plan_id", s.plan.get("plan_id").clone())?;
            Ok(view)
        })
    }

    fn require_landing_review(
        &self,
        plan: &Value,
        task: &Value,
        expected: &ReviewFence,
    ) -> Result<()> {
        expected.require_task(plan, task, "review")?;
        self.verify_review_evidence(task)?;
        require(
            json::hash(task)? == expected.lease.0.text("task_sha256")
                && self.git().text(&["rev-parse", "HEAD"])? == expected.base_sha()
                && json::sha(&self.evidence_read(task.text("artifact"), MAX_PATCH)?)
                    == expected.report_sha256
                && json::sha(&self.evidence_read(task.text("patch"), MAX_PATCH)?)
                    == expected.patch_sha256(),
            "Plan landing review, report, patch or base changed",
        )
    }

    pub fn check_landing_done(
        &self,
        snapshot: &Snapshot,
        task_id: &str,
        expected: Option<&ReviewFence>,
        patch_sha256: &str,
    ) -> Result<()> {
        self.check_landing_result(
            &snapshot.plan,
            snapshot.task(task_id)?,
            expected,
            patch_sha256,
        )
    }

    pub(crate) fn check_landing_result(
        &self,
        plan: &Value,
        task: &Value,
        expected: Option<&ReviewFence>,
        patch_sha256: &str,
    ) -> Result<()> {
        let Some(expected) = expected else {
            return require(
                task.opt(LANDING_REVIEW_SHA256).is_none(),
                "bound Plan completion requires its exact review proof",
            );
        };
        expected.require_task(plan, task, "done")?;
        expected.require_landing_stamp(task)?;
        require(
            patch_sha256 == expected.patch_sha256()
                && json::sha(&self.evidence_read(task.text("artifact"), MAX_PATCH)?)
                    == expected.report_sha256,
            "Plan landing completed with different report or patch evidence",
        )
    }

    fn land(&self, req: &Request, t: &mut Value, plan: &Value) -> Result<()> {
        let review = req
            .has("expected-review-binding")
            .then(|| ReviewFence::parse(req.value("expected-review-binding")?.as_bytes()))
            .transpose()?;
        if let Some(expected) = &review {
            self.require_landing_review(plan, t, expected)?;
        }
        require(
            t.text("state") == "review",
            format!(
                "task {} is {}; only reviewed work can enter landing",
                t.text("id"),
                t.text("state")
            ),
        )?;
        for name in [
            "expected-review-patch",
            "expected-review-patch-sha256",
            "expected-review-verify",
            "expected-review-executor-id",
            "expected-review-executor-soul-sha256",
            "expected-review-lease-id",
        ] {
            require(
                req.has(name),
                format!("land requires complete expected review receipt: {name}"),
            )?;
        }
        let lease = req.value("lease-id")?;
        self.lease(req, t)?;
        self.verify_review_evidence(t)?;
        require(
            req.value("expected-review-lease-id")? == lease && t.text("review_lease_id") == lease,
            "review patch lease mismatch; patch is stale",
        )?;
        require(
            !t.text("artifact").is_empty() && !t.text("patch").is_empty(),
            "review is missing artifact/patch evidence",
        )?;
        let path = req.value("expected-review-patch")?;
        require(
            t.text("patch") == path,
            "reviewed patch path changed during admission",
        )?;
        let expected = req.value("expected-review-patch-sha256")?;
        require(
            json::hex(expected, 64),
            "--expected-review-patch-sha256 must be lowercase SHA-256",
        )?;
        require(
            review
                .as_ref()
                .is_none_or(|review| review.patch_sha256() == expected),
            "Plan landing patch differs from its exact reviewed binding",
        )?;
        self.delegate_review_evidence(t, expected, review.as_ref().map(ReviewFence::base_sha))?;
        require(
            json::sha(self.evidence_read(path, MAX_PATCH)?) == expected,
            "reviewed patch bytes changed during admission",
        )?;
        require(
            t.text("verify").replace('\r', "")
                == req.text("expected-review-verify")?.replace('\r', ""),
            "verify contract changed during admission",
        )?;
        let executor = req.text("expected-review-executor-id")?;
        let soul = req.text("expected-review-executor-soul-sha256")?;
        require(
            executor.is_empty() == soul.is_empty()
                && (executor.is_empty() || valid_id(executor))
                && (soul.is_empty() || json::hex(soul, 64)),
            "expected review executor receipt requires valid id and soul hash",
        )?;
        require(
            t.text("executor_id") == executor && t.text("executor_soul_sha256") == soul,
            "executor review receipt changed during admission",
        )?;
        if let Some(review) = review {
            review.stamp_landing_task(t)?;
        }
        t.set("state", string("landing"))
    }
    fn finish(&self, req: &Request, s: &mut Snapshot, t: &mut Value) -> Result<bool> {
        self.lease(req, t)?;
        self.verify_review_evidence(t)?;
        if let Some(commit) = req.values.get("landed-commit") {
            let commit = commit
                .to_str()
                .ok_or_else(|| error("full landed commit is required"))?;
            let retry = t.text("state") == "done"
                && req.has("lease-id")
                && !req.text("lease-id")?.is_empty();
            let prior = t.get("landing").clone();
            require(
                t.text("state") == "review" || retry,
                format!(
                    "task {} is {}; --landed-commit finish requires review",
                    t.text("id"),
                    t.text("state")
                ),
            )?;
            if retry {
                require(
                    prior.text("sha") == commit && json::hex(prior.text("receipt_sha256"), 64),
                    "completed task does not match exact landed commit/lease",
                )?;
            }
            if let Some(review) = t.opt("review_evidence") {
                require(
                    t.opt("panel_accept").is_some_and(|record| {
                        record.get("patch_sha256") == review.get("patch").get("sha256")
                    }),
                    "exact reviewed evidence requires patch-land unless panel acceptance binds the same patch",
                )?;
            }
            evidence(req, t)?;
            self.verify_review_evidence(t)?;
            let proof = crate::completion::commit_landing(self, commit, None)?;
            if let Some(record) = t.opt("panel_accept") {
                require(
                    t.text("review_lease_id") == t.text("lease_id"),
                    "panel acceptance differs from reviewed lease",
                )?;
                self.panel_accept_record(t.text("id"), t, &s.plan, record, req, true)?;
                let patch = self.evidence_read(record.text("patch"), MAX_PATCH)?;
                let product = self.services.publication_product(
                    &self.context.execution_repo,
                    commit,
                    record,
                    &patch,
                )?;
                require(
                    product.commit == commit
                        && product.patch_sha256 == record.text("patch_sha256")
                        && product.accepted_tree == record.text("tree")
                        && product.base == record.text("base")
                        && !product.raw_proof.is_empty(),
                    "published SHA does not contain exact accepted patch",
                )?;
            }
            let allowed = t.get("allowed_paths").strings()?;
            if !allowed.is_empty() {
                let changed = crate::completion::landed_paths(self, &proof)?;
                let patterns = crate::engine::clean_paths(
                    &allowed.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )?;
                let mut touched = false;
                for path in changed {
                    for p in &patterns {
                        touched |= oms_contract_markdown::scope::matches(&path, p)
                            .map_err(|e| error(e.to_string()))?;
                    }
                }
                require(touched, "landed range changed none of task allowed_paths")?;
            }
            if retry {
                require(
                    proof.landing.get("kind") == prior.get("kind")
                        && proof.landing.get("sha") == prior.get("sha"),
                    "completed task landing proof changed",
                )?;
                return Ok(true);
            }
            // Only issue-bound tasks gain PR evidence from an accepted receipt
            // whose target repo and actual GitHub PR URL match origin; any
            // unprovable, unaccepted, cross-repo or non-PR receipt is omitted.
            if let Some(issue) = t
                .opt("issue")
                .and_then(|v| v.str().ok())
                .filter(|s| !s.is_empty())
                && let Ok(Ok(origin)) = resolve_origin(self)
                && receipt_remote_compatible(self, &proof.receipt, &origin)
                && let Some(pr) = verified_pr_from_receipt(&proof.receipt, issue, &origin)
            {
                t.set("pr", string(pr.to_string()))?;
            }
            history(
                t,
                object([
                    ("schema", number(1)),
                    ("kind", string("finish")),
                    ("ts", string(&self.context.ts)),
                    ("state", string("done")),
                    ("landing", proof.landing.clone()),
                ]),
            )?;
            t.set("landing", proof.landing)?;
            t.set("state", string("done"))?;
            return Ok(false);
        }
        require(
            t.text("state") == "landing",
            format!(
                "task {} is {}; finish only after reviewed work enters landing",
                t.text("id"),
                t.text("state")
            ),
        )?;
        let review = self.require_landing_transition(req, &s.plan, t)?;
        let mut done = t.clone();
        evidence(req, &mut done)?;
        done.set("state", string("done"))?;
        if let Some(review) = review {
            require(
                json::sha(&self.evidence_read(t.text("artifact"), MAX_PATCH)?)
                    == review.report_sha256
                    && done.get("artifact") == t.get("artifact")
                    && json::sha(&self.evidence_read(done.text("patch"), MAX_PATCH)?)
                        == review.patch_sha256()
                    && json::receipt_digest(&done)?
                        == req.value("expected-landing-done-receipt-sha256")?,
                "Plan Finish evidence or expected done receipt changed",
            )?;
        }
        self.verify_review_evidence(&done)?;
        if req.has("refreeze-acceptance") {
            self.refreeze(req, s, t, req.value("expected-landing-receipt-sha256")?)?;
        }
        *t = done;
        Ok(false)
    }
    fn require_landing_transition(
        &self,
        req: &Request,
        plan: &Value,
        task: &Value,
    ) -> Result<Option<ReviewFence>> {
        let receipt = req.value("expected-landing-receipt-sha256")?;
        require(
            task.text("state") == "landing"
                && json::hex(receipt, 64)
                && json::receipt_digest(task)? == receipt,
            "landing receipt changed; stale transition rejected",
        )?;
        let proof = req
            .has("expected-review-binding")
            .then(|| ReviewFence::parse(req.value("expected-review-binding")?.as_bytes()))
            .transpose()?;
        require(
            proof.is_some() == task.opt(LANDING_REVIEW_SHA256).is_some(),
            "bound Plan transition requires its exact review binding",
        )?;
        if let Some(proof) = &proof {
            proof.require_task(plan, task, "landing")?;
            proof.require_landing_stamp(task)?;
            require(
                self.git().text(&["rev-parse", "HEAD"])? == proof.base_sha(),
                "Plan landing base changed before terminal transition",
            )?;
        }
        Ok(proof)
    }
    fn capture_review_evidence(&self, req: &Request, task: &Value) -> Result<Option<Value>> {
        let flags = [
            "expected-review-artifact-sha256",
            "expected-review-patch-sha256",
        ];
        if flags.iter().all(|flag| !req.has(flag)) {
            return Ok(None);
        }
        require(
            flags.iter().all(|flag| req.has(flag)) && !task.text("lease_id").is_empty(),
            "exact review evidence requires both artifact and patch digests under a claim lease",
        )?;
        let mut record = object([
            ("schema", number(1)),
            ("task_id", task.get("id").clone()),
            ("lease_id", task.get("lease_id").clone()),
        ]);
        for (field, flag, limit) in [
            ("artifact", flags[0], MAX_JSON),
            ("patch", flags[1], MAX_PATCH),
        ] {
            let path = req.value(field)?;
            let expected = req.value(flag)?;
            require(
                json::hex(expected, 64),
                "invalid exact review evidence digest",
            )?;
            require(
                json::sha(self.evidence_read(path, limit)?) == expected,
                format!("reviewed {field} bytes changed before review"),
            )?;
            record.set(
                field,
                object([("path", string(path)), ("sha256", string(expected))]),
            )?;
        }
        Ok(Some(record))
    }

    /// Opt-in reviews carry their original evidence through ordinary external
    /// admission. A landing may relocate the exact patch into its frozen store.
    pub(crate) fn verify_review_evidence(&self, task: &Value) -> Result<()> {
        let Some(record) = task.opt("review_evidence") else {
            return Ok(());
        };
        json::keys(
            record,
            &["schema", "task_id", "lease_id", "artifact", "patch"],
            &[],
        )?;
        require(
            record.get("schema").is_int(1)
                && record.get("task_id") == task.get("id")
                && !task.text("lease_id").is_empty()
                && record.get("lease_id") == task.get("lease_id")
                && record.get("lease_id") == task.get("review_lease_id")
                && ["review", "landing", "done", "blocked", "cancelled"]
                    .contains(&task.text("state")),
            "review evidence belongs to another task, lease or state",
        )?;
        let cancelled = task.text("state") == "cancelled";
        if cancelled {
            crate::decision::validate(&self.store, task)?;
        }
        for (field, limit) in [("artifact", MAX_JSON), ("patch", MAX_PATCH)] {
            let reference = record.get(field);
            json::keys(reference, &["path", "sha256"], &[])?;
            let original = reference.get("path").str()?;
            let current = task.get(field).str()?;
            let expected = json::digest(reference.get("sha256"))?;
            require(
                !original.is_empty()
                    && !current.is_empty()
                    && (current == original
                        || field == "patch"
                            && ["done", "blocked", "cancelled"].contains(&task.text("state"))),
                format!("reviewed {field} path changed"),
            )?;
            if cancelled {
                continue;
            }
            require(
                json::sha(self.evidence_read(current, limit)?) == expected,
                format!("reviewed {field} bytes changed"),
            )?;
        }
        Ok(())
    }
    pub(crate) fn evidence_read(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let raw_path = Path::new(path);
        let absolute = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            self.context.state_repo.join(raw_path)
        };
        if absolute.parent()
            == Some(
                self.context
                    .state_repo
                    .join(".oms/landing-patches")
                    .as_path(),
            )
            && let Some(bytes) =
                self.services
                    .read_managed_patch(&self.context.state_repo, &absolute, limit)?
        {
            return Ok(bytes);
        }
        if raw_path.is_absolute() && !raw_path.starts_with(self.store.repo()) {
            let parent = raw_path
                .parent()
                .ok_or_else(|| error("evidence path has no parent"))?;
            let root =
                Store::open(parent)?.with_group_resolver(self.services.group_membership_resolver());
            root.read(
                raw_path
                    .file_name()
                    .ok_or_else(|| error("evidence path has no file name"))?,
                limit,
            )
        } else {
            let rel = raw_path.strip_prefix(self.store.repo()).unwrap_or(raw_path);
            self.store.read(rel, limit)
        }
    }
    fn check_managed_patch(&self, path: &str) -> Result<()> {
        let raw_path = Path::new(path);
        let absolute = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            self.context.state_repo.join(raw_path)
        };
        if absolute.parent()
            == Some(
                self.context
                    .state_repo
                    .join(".oms/landing-patches")
                    .as_path(),
            )
        {
            require(
                [".oms/plan", ".oms/landings", ".oms/artifacts"]
                    .iter()
                    .any(|p| {
                        self.context
                            .plan_file
                            .starts_with(self.context.state_repo.join(p))
                    }),
                "a managed landing capture can only be referenced by a canonical retained plan",
            )?;
            if self
                .services
                .read_managed_patch(&self.context.state_repo, &absolute, MAX_PATCH)?
                .is_none()
            {
                let rel = absolute.strip_prefix(self.store.repo()).unwrap_or(raw_path);
                self.store.read(rel, MAX_PATCH)?;
            }
        }
        Ok(())
    }
    fn refreeze(&self, req: &Request, s: &mut Snapshot, t: &Value, expected: &str) -> Result<()> {
        let mut contract = s.plan.get("project_contract").clone();
        if matches!(contract, Value::Null) {
            return Ok(());
        }
        let patch = if req.text("patch")?.is_empty() {
            t.text("patch")
        } else {
            req.text("patch")?
        };
        if patch.is_empty() {
            return Ok(());
        }
        let raw = self.evidence_read(patch, MAX_PATCH)?;
        let mut touched = BTreeSet::new();
        for line in String::from_utf8_lossy(&raw).lines() {
            if let Some(name) = line.strip_prefix("+++ b/") {
                touched.insert(name.trim().to_owned());
            } else if line.starts_with("diff --git a/")
                && let Some((_, name)) = line.rsplit_once(" b/")
            {
                touched.insert(name.trim().to_owned());
            }
        }
        let files = contract
            .get("acceptance_files")
            .strings()?
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut manifest = contract.get("acceptance_manifest").clone();
        let mut entries = Vec::new();
        for (i, rel) in files.iter().enumerate() {
            if !touched.contains(rel) {
                continue;
            }
            let Some(row) = manifest.array_mut()?.get_mut(i) else {
                continue;
            };
            let new = json::sha(self.source.read(rel, 8 * 1024 * 1024)?);
            let old = row.text("sha256").to_owned();
            if new != old {
                *row = object([("path", string(rel)), ("sha256", string(&new))]);
                entries.push(object([
                    ("path", string(rel)),
                    ("old", string(old)),
                    ("new", string(new)),
                ]));
            }
        }
        if !entries.is_empty() {
            let row = object([
                ("schema", number(1)),
                ("kind", string("manifest-refreeze")),
                ("task", t.get("id").clone()),
                ("landing_receipt_sha256", string(expected)),
                ("entries", Value::Array(entries)),
                ("ts", string(&self.context.ts)),
            ]);
            self.store
                .append(".oms/plan/manifest-refreeze.jsonl", &row)?;
            contract.set("acceptance_manifest", manifest)?;
            s.plan.set("project_contract", contract)?;
        }
        Ok(())
    }
    /// Reopens a saved acceptance and its original candidates. Resource
    /// producers call this while retaining the canonical root/Plan guards;
    /// neither a caller record nor an indexed patch alone is a disposition.
    pub fn accepted_panel_candidates(&self, expected: &Value, owner: &str) -> Result<Vec<Value>> {
        self.context.parent()?;
        require(
            self.context.canonical(),
            "acceptance requires the canonical plan",
        )?;
        let snapshot = self.load()?;
        let task = snapshot.task(expected.text("task_id"))?;
        require(
            ["review", "done"].contains(&task.text("state"))
                && task.get("panel_accept") == expected
                && !task.text("lease_id").is_empty()
                && task.get("review_lease_id") == task.get("lease_id")
                && task.text("claimed_by_participant") == self.context.participant
                && self.context.participant == self.context.panel_main_attempt,
            "accepted candidate task, owner or reviewed lease changed",
        )?;
        self.panel_accept_record(
            task.text("id"),
            task,
            &snapshot.plan,
            expected,
            &Request::new(Action::Status),
            true,
        )?;
        let lifecycle = crate::provenance::lifecycle_projection(
            self,
            &self
                .store
                .read(".oms/lifecycle/events.jsonl", storage::MAX_STREAM)?,
        )?;
        require(
            lifecycle
                .get(&self.context.panel_main_attempt)
                .text("provider")
                == owner,
            "accepted candidate provider owner changed",
        )?;
        let rows = self.store.rows(".oms/artifacts/index.jsonl", false)?;
        let indexed = |id: &str, kind: &str| -> Result<&Value> {
            let mut found = rows.iter().filter(|row| row.text("event_id") == id);
            let row = found
                .next()
                .ok_or_else(|| error("accepted candidate evidence is missing"))?;
            require(
                row.text("kind") == kind && found.next().is_none(),
                "accepted candidate evidence is ambiguous",
            )?;
            Ok(row)
        };
        let components = if expected.get("schema").is_int(2) {
            json::parse(&self.evidence_read(expected.text("artifact"), MAX_JSON)?)?
                .get("components")
                .array()?
                .iter()
                .map(|component| {
                    (
                        component.get("candidate").clone(),
                        component.get("review_gate").clone(),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            vec![(
                indexed(expected.text("candidate_event_id"), "delegate")?.clone(),
                expected.get("review_gate").clone(),
            )]
        };
        let mut result = Vec::new();
        for (candidate, gate) in components {
            let attempt = candidate.text("attempt_id");
            let worker = lifecycle.get(attempt);
            require(
                indexed(candidate.text("event_id"), "delegate")? == &candidate
                    && candidate.get("exit").is_int(0)
                    && candidate.get("plan_id") == expected.get("plan_id")
                    && candidate.get("task_id") == expected.get("task_id")
                    && candidate.text("base_sha") == expected.text("base")
                    && worker.get("task_id") == expected.get("task_id")
                    && worker.text("parent_attempt_id") == self.context.panel_main_attempt
                    && worker.get("refs").get("panel_lease_id") == expected.get("lease_id")
                    && worker.get("refs").text("panel_role") == "worker"
                    && worker.get("refs").text("panel_access") == "write"
                    && (worker.get("terminal") == &Value::Bool(true)
                        || worker.text("state") == "review"),
                "accepted candidate worker, task or lease differs from its canonical evidence",
            )?;
            if expected.get("schema").is_int(1) {
                require(
                    candidate.text("attempt_id") == expected.text("candidate_attempt_id")
                        && ["patch", "patch_sha256", "artifact"]
                            .iter()
                            .all(|key| candidate.get(key) == expected.get(key)),
                    "accepted candidate differs from the saved acceptance",
                )?;
            }
            for (field, hash) in [("artifact", "artifact_sha256"), ("patch", "patch_sha256")] {
                let path = candidate.text(field);
                require(
                    path.starts_with(".oms/artifacts/")
                        && clean_path(path)? == path
                        && json::sha(self.evidence_read(
                            path,
                            if field == "patch" {
                                MAX_PATCH
                            } else {
                                MAX_JSON
                            },
                        )?) == candidate.text(hash),
                    "accepted candidate artifact changed",
                )?;
            }
            let review = indexed(gate.text("event_id"), "review-outcome")?;
            let seats = review.get("review").get("seats").array()?;
            require(
                review.get("task_id") == expected.get("task_id")
                    && review.get("exit").is_int(0)
                    && review.get("review").get("gate_verify_exit").is_int(0)
                    && review.get("review").get("candidate").get("patch_sha256")
                        == candidate.get("patch_sha256")
                    && !seats.is_empty()
                    && seats.iter().all(|seat| seat.text("verdict") == "pass")
                    && gate.get("seat_verdicts").strings()?
                        == seats
                            .iter()
                            .map(|seat| seat.text("verdict"))
                            .collect::<Vec<_>>(),
                "accepted candidate review evidence changed",
            )?;
            require(
                rows.iter().any(|row| {
                    row.text("kind") == "panel-accept"
                        && row.get("task_id") == expected.get("task_id")
                        && row.get("plan_id") == expected.get("plan_id")
                        && row.text("attempt_id") == attempt
                        && row.get("patch_sha256") == candidate.get("patch_sha256")
                        && row.text("base_sha") == expected.text("commit")
                        && row.get("exit").is_int(0)
                }),
                "accepted candidate registration is missing",
            )?;
            result.push(candidate);
        }
        Ok(result)
    }

    fn panel_accept_record(
        &self,
        id: &str,
        t: &Value,
        plan: &Value,
        record: &Value,
        req: &Request,
        from_task: bool,
    ) -> Result<()> {
        let composed = record.get("schema").is_int(2);
        json::keys(
            record,
            &[
                "schema",
                "task_id",
                "plan_id",
                "lease_id",
                "candidate_event_id",
                "candidate_attempt_id",
                "patch_sha256",
                "artifact",
                "patch",
                "commit",
                "tree",
                "base",
                "review_gate",
            ],
            if composed {
                &["composition", "integration_verify"]
            } else {
                &[]
            },
        )?;
        require(
            (record.get("schema").is_int(1) || composed)
                && record.text("task_id") == id
                && record.get("plan_id") == plan.get("plan_id")
                && record.get("lease_id") == t.get("lease_id")
                && receipt_id(record.text("candidate_event_id"), 160)
                && receipt_id(record.text("candidate_attempt_id"), 160)
                && json::hex(record.text("patch_sha256"), 64),
            "panel acceptance record does not match current task, plan or lease",
        )?;
        let gate = record.get("review_gate");
        require(
            gate.get("patch_sha256") == record.get("patch_sha256")
                && gate.get("event_id").str().is_ok()
                && gate.get("exit").is_int(0)
                && (if composed {
                    gate.text("kind") == "component-reviews"
                } else {
                    gate.get("gate_verify_exit").is_int(0)
                })
                && !gate.get("seat_verdicts").array()?.is_empty()
                && gate
                    .get("seat_verdicts")
                    .strings()?
                    .iter()
                    .all(|s| *s == "pass"),
            "panel acceptance review gate is not successful",
        )?;
        for name in ["commit", "tree", "base"] {
            require(
                crate::repository::commit(record.text(name)),
                "panel acceptance full commit identities required",
            )?;
        }
        for name in ["artifact", "patch"] {
            let value = record.get(name).str()?;
            require(
                clean_path(value)? == value
                    && value
                        == if from_task {
                            t.text(name)
                        } else {
                            req.text(name)?
                        },
                "panel acceptance differs from reviewed evidence",
            )?;
        }
        let patch = self.evidence_read(record.text("patch"), MAX_PATCH)?;
        require(
            json::sha(&patch) == record.text("patch_sha256"),
            "panel acceptance patch digest changed",
        )?;
        if composed {
            crate::panel_composition::validate(self, t, record, &patch)?;
        }
        let index = crate::repository::ProductIndex::new(&self.git())?;
        index.run(&self.git(), &["read-tree", record.text("base")], None)?;
        index.run(
            &self.git(),
            &["apply", "--cached", "--binary", "-"],
            Some(&patch),
        )?;
        require(
            crate::repository::trim(&index.run(&self.git(), &["write-tree"], None)?)
                == record.text("tree").as_bytes(),
            "panel acceptance tree does not exactly contain reviewed patch",
        )?;
        let found = self.git().output(
            &[
                std::ffi::OsStr::new("cat-file"),
                std::ffi::OsStr::new("-e"),
                std::ffi::OsStr::new(&format!("{}^{{commit}}", record.text("commit"))),
            ],
            None,
            &[],
            4096,
        )?;
        if found.code == 0 {
            let commit = record.text("commit");
            let parents = self
                .git()
                .text(&["rev-list", "--parents", "-n", "1", commit])?;
            let parents = parents.split_whitespace().collect::<Vec<_>>();
            require(
                parents.len() == 2
                    && parents[0] == commit
                    && parents[1] == record.text("base")
                    && self
                        .git()
                        .text(&["rev-parse", &format!("{commit}^{{tree}}")])?
                        == record.text("tree"),
                "panel acceptance commit tree differs from reviewed patch",
            )?;
        } else {
            require(
                found.code == 1 && from_task,
                "panel acceptance commit is unavailable",
            )?;
        }
        Ok(())
    }
}
pub(crate) fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub(crate) fn receipt_id(s: &str, limit: usize) -> bool {
    !s.is_empty()
        && s.len() <= limit
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
pub(crate) fn lineage(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|s| json::hex(s, 32))
}
pub(crate) fn room_id(s: &str) -> bool {
    s.len() <= 160 && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric) && valid_id(s)
}
pub(crate) fn plain(s: &str, label: &str) -> Result<()> {
    require(
        !s.chars().any(|c| {
            matches!(
                get_general_category(c),
                GeneralCategory::Control | GeneralCategory::Format | GeneralCategory::Surrogate
            )
        }),
        format!("{label} contains a control or format character"),
    )
}
pub(crate) fn screen(raw: &[u8]) -> Result<()> {
    let hits = oms_scrub::scan_secret(raw, &[])
        .map_err(|_| error("persistent-content screen unavailable"))?;
    require(hits.is_empty(), "persistent content looks sensitive")
}
pub(crate) fn reason<'a>(req: &'a Request, noun: &str) -> Result<&'a str> {
    let reason = req.value("reason")?;
    plain(reason, &format!("{noun} reason"))?;
    require(
        reason.chars().count() <= 500,
        format!("{noun} reason must be 1..500 characters"),
    )?;
    Ok(reason)
}
pub(crate) fn split_list(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
pub(crate) fn clean_path(s: &str) -> Result<String> {
    oms_contract_markdown::scope::normalize(s).map_err(|e| error(e.to_string()))
}
pub(crate) fn clean_paths(v: &[String]) -> Result<Vec<String>> {
    v.iter().map(|s| clean_path(s)).collect()
}
pub(crate) fn lint(command: &str, allowed: &[String]) -> Result<()> {
    let hits = crate::floor::incompatible_reads(
        command,
        &allowed.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    if let Some((p, via)) = hits.first() {
        return Err(error(format!(
            "floor_incompatible_verifier: task verify reads {p} ({via})"
        )));
    }
    Ok(())
}
pub(crate) fn ensure_lineage(d: &mut Value) -> Result<String> {
    if d.opt("plan_id").is_none() {
        d.set(
            "plan_id",
            string(format!("plan_{}", storage::random_hex(16)?)),
        )?;
    }
    let id = d.get("plan_id").str()?;
    require(
        lineage(id, "plan_"),
        "plan_id is malformed; refusing to replace immutable lineage",
    )?;
    Ok(id.into())
}
pub(crate) fn history(t: &mut Value, row: Value) -> Result<()> {
    let mut h = t.opt("history").cloned().unwrap_or(Value::Array(vec![]));
    h.array_mut()?.push(row);
    t.set("history", h)
}
pub(crate) fn evidence(req: &Request, t: &mut Value) -> Result<()> {
    for name in ["artifact", "patch"] {
        if !req.text(name)?.is_empty() {
            t.set(name, string(req.text(name)?))?;
        }
    }
    Ok(())
}
pub(crate) fn clear_claim(s: &mut Snapshot, t: &mut Value) -> Result<()> {
    if !t.text("lease_id").is_empty() {
        s.settled.insert(t.text("lease_id").into());
    }
    for (name, value) in [
        ("state", "ready"),
        ("provider", ""),
        ("ttl", ""),
        ("claimed_at", ""),
        ("reason", ""),
        ("lease_id", ""),
        ("autopilot_owner_id", ""),
    ] {
        t.set(name, string(value))?;
    }
    t.remove("claimed_by_participant");
    t.remove("review_evidence");
    t.remove(LANDING_REVIEW_SHA256);
    Ok(())
}
fn delegate_lock_error(error: oms_lock::Error) -> crate::Error {
    crate::Error {
        code: error.exit_code(),
        message: error.to_string(),
    }
}

fn delegation_time() -> Result<String> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| error("invalid delegation clock"))?;
    Ok(DateTime::<Utc>::from_timestamp(time.as_secs() as i64, 0)
        .ok_or_else(|| error("invalid delegation clock"))?
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string())
}
pub(crate) fn parse_ts(s: &str) -> Option<i128> {
    if s.len() != 20 {
        return None;
    }
    let dt = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ").ok()?;
    if dt.format("%Y-%m-%dT%H:%M:%SZ").to_string() != s {
        return None;
    }
    Some(i128::from(dt.and_utc().timestamp()))
}
pub(crate) fn age(t: &Value, now: &str) -> Option<i128> {
    let anchor = if t.text("state") == "review" {
        parse_ts(t.text("updated"))
    } else {
        parse_ts(t.text("claimed_at")).or_else(|| parse_ts(t.text("updated")))
    }?;
    Some(parse_ts(now)? - anchor)
}
pub(crate) fn task_ttl(t: &Value, default: u64) -> u64 {
    let raw = t.text("ttl");
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        raw.parse().unwrap_or(u64::MAX)
    } else {
        default
    }
}
pub(crate) fn marker_typed(m: &Value) -> bool {
    let schema = m.get("schema").int().unwrap_or(-1);
    let pid = m.get("pid").int().unwrap_or(0);
    (1..=4).contains(&schema)
        && valid_id(m.text("id"))
        && !matches!(m.text("id"), "." | "..")
        && (1..=0x7fffffff).contains(&pid)
        && (schema != 4 || m.opt("native_pid").is_some())
        && m.opt("native_pid")
            .is_none_or(|v| v.int().is_ok_and(|n| (1..=0xffffffff).contains(&n)))
        && ["task_id", "lease_id", "executor_id"].iter().all(|k| {
            m.opt(k)
                .is_none_or(|v| v.str().is_ok_and(|s| s.is_empty() || valid_id(s)))
        })
        && !matches!(m.text("executor_id"), "." | "..")
        && m.opt("access")
            .is_none_or(|v| matches!(v.str(), Ok("read" | "write")))
        && m.opt("autopilot_owner_id")
            .is_none_or(|v| v.str().is_ok_and(|s| s.is_empty() || lineage(s, "owner_")))
}
pub(crate) fn marker_alive(m: &Value) -> bool {
    let Ok(pid) = m.get("pid").int() else {
        return true;
    };
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return true;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(rustix::io::Errno::SRCH) => false,
        Err(_) => true,
    }
}
pub(crate) fn assignment(v: &Value) -> Result<()> {
    if v.object()?.is_empty() {
        return Ok(());
    }
    json::keys(
        v,
        &["provider"],
        &["model", "fallback_model", "reasoning_effort", "workload"],
    )?;
    for (_, item) in v.object()? {
        plain(item.str()?, "assignment")?;
    }
    let p = v.get("provider").str()?;
    require(
        !p.is_empty()
            && p.len() <= 64
            && p.as_bytes()[0].is_ascii_alphanumeric()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            && !p.contains("..")
            && ![
                "agy",
                "cursor-agent",
                "grok-build",
                "gemini-cli",
                "qwen-code",
                "opencode2",
                "dsh",
                "deepseek-harness",
                "mistral-vibe",
                "github-copilot",
                "factory-droid",
                "kiro-cli",
                "cn",
            ]
            .contains(&p),
        "assignment provider must be a canonical transport identifier",
    )?;
    for name in ["model", "fallback_model"] {
        let s = v.text(name);
        require(
            s.chars().count() <= 160
                && !s.starts_with('-')
                && s == s.trim()
                && s != "provider-default",
            format!("invalid assignment {name}"),
        )?;
    }
    require(
        v.opt("workload")
            .is_none_or(|v| matches!(v.str(), Ok("standard" | "routine"))),
        "assignment workload must be standard or routine",
    )?;
    require(
        v.opt("reasoning_effort").is_none_or(|v| {
            matches!(
                v.str(),
                Ok("auto" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra")
            )
        }),
        "invalid assignment reasoning_effort",
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_task(
    id: &str,
    title: &str,
    depends: Vec<String>,
    allowed: Vec<String>,
    forbidden: Vec<String>,
    verify: &str,
    role: &str,
    assignment: Value,
    ts: &str,
) -> Result<Value> {
    Ok(object([
        ("id", string(id)),
        ("title", string(title)),
        ("state", string("ready")),
        ("depends", list(depends.iter().map(String::as_str))),
        ("allowed_paths", list(allowed.iter().map(String::as_str))),
        (
            "forbidden_paths",
            list(forbidden.iter().map(String::as_str)),
        ),
        ("verify", string(verify)),
        ("role", string(role)),
        ("assignment", assignment),
        ("provider", string("")),
        ("ttl", string("")),
        ("artifact", string("")),
        ("patch", string("")),
        ("reason", string("")),
        ("executor_id", string("")),
        ("executor_soul_sha256", string("")),
        ("autopilot_owner_id", string("")),
        ("lease_epoch", number(0)),
        ("lease_id", string("")),
        ("review_lease_id", string("")),
        ("repair_count", number(0)),
        ("repair_artifact", string("")),
        ("created", string(ts)),
        ("updated", string(ts)),
    ]))
}
/// The local `origin` remote: inner `Err` when it is missing or not a
/// github.com URL, outer `Err` only when Git itself failed or timed out.
/// Diagnostics never echo the URL.
type Remote = std::result::Result<std::result::Result<oms_github::Slug, String>, String>;
pub(crate) fn resolve_origin(plan: &Plan<'_>) -> Remote {
    let args = ["remote", "get-url", "origin"].map(std::ffi::OsStr::new);
    let out = plan
        .git()
        .output(&args, None, &[], 64 * 1024)
        .map_err(|_| "github issue: origin remote could not be read".to_owned())?;
    if out.code != 0 {
        return Ok(Err(
            "github issue: requires an origin remote; none is configured".into(),
        ));
    }
    Ok(std::str::from_utf8(crate::repository::trim(&out.stdout))
        .map_err(|_| "github issue: remote URL is empty or malformed".to_owned())
        .and_then(|url| {
            oms_github::Slug::from_remote(url)
                .map_err(|e| format!("github issue: requires a github.com origin: {}", e.message))
        }))
}

/// Parses an exact GitHub pull request URL (`https://github.com/<owner>/<repo>/pull/<number>`)
/// bound to the normalized target repository `origin`. Rejects issue URLs,
/// non-GitHub hosts, cross-repo URLs, trailing segments, queries, fragments,
/// and non-positive or malformed PR numbers.
fn parse_github_pr_url(raw: &str, origin: &oms_github::Slug) -> Option<oms_github::PullRef> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let rest = raw.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    if path.contains(['?', '#', '@', '\\']) {
        return None;
    }
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let kind = parts.next()?;
    let number = parts.next()?;
    if parts.next().is_some() || kind != "pull" {
        return None;
    }
    let slug = oms_github::Slug::parse(&format!("{owner}/{repo}")).ok()?;
    if !slug.matches(origin) {
        return None;
    }
    let number = oms_github::Number::parse(number).ok()?;
    Some(oms_github::PullRef {
        slug: origin.clone(),
        number,
    })
}

/// Verifies that the receipt's pushed remote has a single GitHub push
/// destination matching `origin` (mirroring landing/delivery's `--push`
/// resolution, even when fetch and push URLs differ).
fn receipt_remote_compatible(plan: &Plan<'_>, receipt: &Value, origin: &oms_github::Slug) -> bool {
    let remote = receipt.text("remote");
    if remote.is_empty()
        || remote.starts_with('-')
        || !remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return false;
    }
    let args = ["remote", "get-url", "--push", "--all", remote].map(std::ffi::OsStr::new);
    let Ok(out) = plan.git().output(&args, None, &[], 64 * 1024) else {
        return false;
    };
    if out.code != 0 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(crate::repository::trim(&out.stdout)) else {
        return false;
    };
    let mut lines = text.lines();
    let (Some(url), None) = (lines.next(), lines.next()) else {
        return false;
    };
    oms_github::Slug::from_remote(url).is_ok_and(|slug| slug.matches(origin))
}

/// Extracts a verified PR reference ONLY from an accepted landing receipt whose
/// target repository and actual GitHub PR URL match `origin` and `pr_number`.
fn verified_pr_from_receipt(
    receipt: &Value,
    issue: &str,
    origin: &oms_github::Slug,
) -> Option<oms_github::PullRef> {
    oms_github::IssueRef::parse(issue, origin).ok()?;
    if !receipt.get("schema").is_int(1) || receipt.get("dry_run") == &Value::Bool(true) {
        return None;
    }
    if !receipt.get("gate").get("rc").is_int(0)
        || !receipt.get("push").get("rc").is_int(0)
        || receipt.get("push").get("moved").is_int(0)
        || receipt.get("ci").text("conclusion") != "success"
    {
        return None;
    }
    let update = receipt.get("update");
    let deferred = update.get("rc").is_int(76) && update.text("reason") == "deferred-live-workers";
    if receipt.text("state") != "passed" && !deferred {
        return None;
    }
    if let Some(merge) = receipt.opt("merge")
        && merge.text("state") != "MERGED"
    {
        return None;
    }
    if let Some(target_repo) = receipt
        .opt("target_repo")
        .and_then(|v| v.str().ok())
        .filter(|s| !s.is_empty())
        && !oms_github::Slug::parse(target_repo).is_ok_and(|s| s.matches(origin))
    {
        return None;
    }
    let pr_url = receipt.opt("pr_url").and_then(|v| v.str().ok())?;
    let pr = parse_github_pr_url(pr_url, origin)?;
    let n = receipt.get("pr_number").int().ok()?;
    let pr_number = oms_github::Number::parse(&n.to_string()).ok()?;
    if pr_number != pr.number {
        return None;
    }
    Some(pr)
}

/// Opt-in GitHub project mode through the shared declaration reader
/// (PROJECT.md plus the tracked `.github/oms-project.md` at HEAD) and its
/// origin guard. Issues own shared work; tasks only reference them. Origin is
/// resolved locally and only when a declaration or an issue is involved.
pub(crate) struct Issues {
    /// The active project; `Err` blocks task creation and issue mutations.
    mode: std::result::Result<Option<oms_github::Slug>, String>,
    origin: std::cell::OnceCell<Remote>,
}
impl Issues {
    pub(crate) fn load(plan: &Plan<'_>) -> Self {
        let mut issues = Self {
            mode: Ok(None),
            origin: std::cell::OnceCell::new(),
        };
        let prefix = |e: oms_github::Error| format!("github issue: {}", e.message);
        let mode = match oms_github::declaration::inspect(&plan.context.execution_repo) {
            Err(e) => Err(prefix(e)),
            Ok(None) => Ok(None),
            Ok(Some(decl)) => match issues.remote(plan) {
                Err(m) => Err(m.clone()),
                Ok(found) => oms_github::declaration::active(Some(&decl), found.as_ref().ok())
                    .map_err(prefix),
            },
        };
        issues.mode = mode;
        issues
    }
    fn remote(&self, plan: &Plan<'_>) -> &Remote {
        self.origin.get_or_init(|| resolve_origin(plan))
    }
    pub(crate) fn origin(&self, plan: &Plan<'_>) -> Result<&oms_github::Slug> {
        match self.remote(plan) {
            Ok(Ok(slug)) => Ok(slug),
            Ok(Err(m)) | Err(m) => Err(error(m)),
        }
    }
    /// Refuses any issue mutation under a failed declaration or without a
    /// github.com origin; an active project already matches origin.
    fn checked(&self, plan: &Plan<'_>) -> Result<&oms_github::Slug> {
        self.mode.as_ref().map_err(error)?;
        self.origin(plan)
    }
    /// Canonical `owner/repo#N` for `N`, `#N` or `owner/repo#N`.
    pub(crate) fn canonical(&self, plan: &Plan<'_>, raw: &str) -> Result<String> {
        let origin = self.checked(plan)?;
        oms_github::IssueRef::parse(raw, origin)
            .map(|r| r.to_string())
            .map_err(|e| error(format!("github issue: {}", e.message)))
    }
    /// Whether new tasks must carry an issue (an active project).
    pub(crate) fn required(&self) -> Result<bool> {
        Ok(self.mode.as_ref().map_err(error)?.is_some())
    }
}
pub(crate) fn project_snapshot(source: &Store) -> Result<Value> {
    let raw = source.optional("PROJECT.md", 1024 * 1024);
    let (present, healthy, state, hash) = match raw {
        Ok(None) => (false, true, "missing".to_owned(), String::new()),
        Ok(Some(raw)) => match std::str::from_utf8(&raw) {
            Ok(text) => {
                let values = text
                    .split('\n')
                    .filter_map(|l| {
                        l.strip_suffix('\r')
                            .unwrap_or(l)
                            .strip_prefix("- State:")
                            .map(str::trim)
                    })
                    .collect::<Vec<_>>();
                let state = match values.as_slice() {
                    [] => "missing",
                    ["draft"] => "draft",
                    ["confirmed"] => "confirmed",
                    ["active"] => "legacy-active",
                    _ => "invalid",
                };
                (true, true, state.into(), json::sha(&raw))
            }
            Err(_) => (true, false, "invalid".into(), String::new()),
        },
        Err(_) => (true, false, "invalid".into(), String::new()),
    };
    Ok(object([
        ("schema", number(1)),
        ("present", Value::Bool(present)),
        ("healthy", Value::Bool(healthy)),
        ("state", string(state)),
        ("sha256", string(hash)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_spawn_rechecks_expiry_and_late_marker_group_after_checkout() {
        use std::cell::Cell;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use std::time::{Duration, Instant};

        struct Repo(PathBuf);
        impl Drop for Repo {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct Host<'a> {
            source: &'a Path,
            mode: &'static str,
            group_reads: Cell<usize>,
        }
        impl Services for Host<'_> {
            fn group_membership_before(
                &self,
                gid: u32,
                deadline: Instant,
                stop: &AtomicBool,
            ) -> Result<crate::services::GroupMembership> {
                Context::before(stop, Some(deadline))?;
                let reads = self.group_reads.get() + 1;
                self.group_reads.set(reads);
                if self.mode == "moved-source" && reads == 1 {
                    let _ = git_at(
                        self.source,
                        &["commit", "--allow-empty", "-qm", "source moved"],
                    );
                }
                let qualified = self.mode != "unproved" && (self.mode != "revoked" || reads == 1);
                let uid = rustix::process::getuid().as_raw();
                Ok(crate::services::GroupMembership {
                    gid,
                    explicit_member_uids: vec![uid],
                    primary_member_uids: if qualified { vec![uid] } else { Vec::new() },
                })
            }
        }

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let repo = Repo(temp.join(format!(
            "oms-plan-spawn-{}",
            storage::random_hex(16).unwrap()
        )));
        for path in ["", ".oms", ".oms/plan", ".oms/delegations"] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(repo.0.join(path))
                .unwrap();
        }
        fn git_at(repo: &Path, args: &[&str]) -> String {
            let output = std::process::Command::new("/bin/sh")
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(repo)
                .args(["-c", "umask 077; exec git -c user.name=Fixture -c user.email=fixture@example.invalid \"$@\"", "--"])
                .args(args)
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }

        let _ = git_at(&repo.0, &["init", "-q"]);
        let _ = git_at(&repo.0, &["commit", "--allow-empty", "-qm", "fixture"]);
        let base = git_at(&repo.0, &["rev-parse", "HEAD"]);
        let _ = git_at(
            &repo.0,
            &["worktree", "add", "-q", "--detach", "worker", &base],
        );
        let worker = repo.0.join("worker");
        let stop = AtomicBool::new(false);
        let mut settings = oms_lock::Settings::from_env().unwrap();
        settings.mode = oms_lock::Mode::Flock;
        settings.cwd = repo.0.clone();
        settings.home = repo.0.clone();
        settings.temp_dir = repo.0.clone();
        settings.lock_dir = None;
        let context = Context {
            cwd: repo.0.clone(),
            execution_repo: repo.0.clone(),
            state_repo: repo.0.clone(),
            plan_file: repo.0.join(ACTIVE),
            ts: delegation_time().unwrap(),
            child: false,
            delegate_depth: String::new(),
            owner_id: String::new(),
            room_id: String::new(),
            participant: String::new(),
            panel_main_attempt: String::new(),
            attempt_id: String::new(),
            harness_call_id: String::new(),
            claim_ttl: 3600,
            run_id: String::new(),
            cycle: None,
            lock_settings: settings,
        };
        let setup = Host {
            source: &repo.0,
            mode: "qualified",
            group_reads: Cell::new(0),
        };
        let plan = Plan::open(context.clone(), &setup, &stop).unwrap();
        plan.execute(&Request::new(Action::Init).with("goal", "fixture"))
            .unwrap();
        plan.execute(
            &Request::new(Action::Add)
                .with("id", "task")
                .with("title", "fixture"),
        )
        .unwrap();
        let issued = plan
            .execute(
                &Request::new(Action::Claim)
                    .with("id", "task")
                    .with("provider", "codex")
                    .flag("json"),
            )
            .unwrap();
        let issued = json::parse(&issued.stdout).unwrap();
        let binding = DelegateBinding::parse(
            &json::canonical(&object([
                ("schema", number(1)),
                ("plan_id", issued.get("plan_id").clone()),
                ("task_id", string("task")),
                ("task_sha256", issued.get("task_sha256").clone()),
                ("lease_id", issued.get("lease_id").clone()),
                ("lease_epoch", issued.get("lease_epoch").clone()),
                ("base_sha", string(&base)),
            ]))
            .unwrap(),
        )
        .unwrap();
        let running = plan
            .execute_bound_task(
                &Request::new(Action::Start)
                    .with("id", "task")
                    .with("lease-id", binding.lease_id()),
                &binding,
                None,
            )
            .unwrap();
        let before = plan.load().unwrap().raw.unwrap();
        let start = DateTime::parse_from_rfc3339(issued.text("claimed_at")).unwrap();
        let expiry = (start + chrono::TimeDelta::seconds(3600))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        for mode in [
            "expired",
            "qualified",
            "unproved",
            "revoked",
            "moved-source",
        ] {
            if mode == "qualified" {
                let marker = json::encoded(&object([
                    ("schema", number(1)),
                    ("id", string("live-worker")),
                    ("pid", number(std::process::id())),
                    ("task_id", string("task")),
                    ("lease_id", string(running.lease_id())),
                ]))
                .unwrap();
                plan.store
                    .publish_once(".oms/delegations/live-worker.json", &marker, 4096)
                    .unwrap();
                std::fs::set_permissions(
                    repo.0.join(".oms/delegations"),
                    std::fs::Permissions::from_mode(0o770),
                )
                .unwrap();
            }
            let host = Host {
                source: &repo.0,
                mode,
                group_reads: Cell::new(0),
            };
            let checkouts = Cell::new(0);
            let spawned = Cell::new(false);
            let result = Plan::open(context.clone(), &host, &stop)
                .unwrap()
                .with_running_delegate_before_clock(
                    &running,
                    "codex",
                    Instant::now() + Duration::from_secs(10),
                    || {
                        assert_eq!(2 * checkouts.get(), host.group_reads.get());
                        checkouts.set(checkouts.get() + 1);
                        if mode == "moved-source" {
                            assert_eq!(git_at(&worker, &["rev-parse", "HEAD"]), base);
                            assert!(
                                git_at(&worker, &["diff", "--cached", "--name-only"]).is_empty()
                            );
                        }
                        Ok::<(), crate::Error>(())
                    },
                    || {
                        spawned.set(true);
                        Ok(())
                    },
                    || {
                        assert!(checkouts.get() > 0);
                        Ok(expiry.clone())
                    },
                );
            match mode {
                "qualified" => {
                    result.unwrap().unwrap();
                    assert!(spawned.get());
                    assert_eq!(host.group_reads.get(), 2);
                    assert_eq!(checkouts.get(), 2);
                }
                "unproved" => {
                    assert!(result.is_err());
                    assert!(!spawned.get());
                    assert_eq!(host.group_reads.get(), 2);
                    assert_eq!(checkouts.get(), 2);
                }
                "revoked" | "moved-source" => {
                    let expected = if mode == "revoked" {
                        "native group ownership changed before provider start"
                    } else {
                        assert_ne!(git_at(&repo.0, &["rev-parse", "HEAD"]), base);
                        assert_eq!(git_at(&worker, &["rev-parse", "HEAD"]), base);
                        assert!(git_at(&worker, &["diff", "--cached", "--name-only"]).is_empty());
                        "Plan delegation task, lease, scope or base changed"
                    };
                    assert_eq!(result.unwrap_err().message, expected);
                    assert!(!spawned.get());
                    assert_eq!(host.group_reads.get(), 2);
                    assert_eq!(checkouts.get(), 1);
                }
                _ => {
                    assert_eq!(
                        result.unwrap_err().message,
                        "Plan delegation lease expired before provider start"
                    );
                    assert!(!spawned.get());
                    assert_eq!(host.group_reads.get(), 0);
                    assert_eq!(checkouts.get(), 1);
                }
            }
        }
        assert_eq!(plan.load().unwrap().raw.unwrap(), before);
    }

    fn origin() -> oms_github::Slug {
        oms_github::Slug::parse("example-org/example-repo").unwrap()
    }

    fn accepted_receipt() -> Value {
        object([
            ("schema", number(1)),
            ("state", string("passed")),
            ("remote", string("origin")),
            ("target", string("main")),
            ("gate", object([("rc", number(0))])),
            ("push", object([("rc", number(0)), ("moved", number(1))])),
            ("ci", object([("conclusion", string("success"))])),
            (
                "merge",
                object([("method", string("squash")), ("state", string("MERGED"))]),
            ),
            ("pr_number", number(19)),
            (
                "pr_url",
                string("https://github.com/Example-Org/Example-Repo/pull/19"),
            ),
        ])
    }

    #[test]
    fn verified_pr_from_receipt_accepts_only_accepted_same_repo_pr_url() {
        let origin = origin();
        let receipt = accepted_receipt();
        let pr = verified_pr_from_receipt(&receipt, "example-org/example-repo#7", &origin)
            .expect("accepted same-repo PR receipt");
        assert_eq!(pr.to_string(), "example-org/example-repo#19");

        // Deferred live-worker install state is also accepted.
        let mut deferred = accepted_receipt();
        deferred.set("state", string("deferred")).unwrap();
        deferred
            .set(
                "update",
                object([
                    ("rc", number(76)),
                    ("reason", string("deferred-live-workers")),
                ]),
            )
            .unwrap();
        assert_eq!(
            verified_pr_from_receipt(&deferred, "example-org/example-repo#7", &origin)
                .unwrap()
                .to_string(),
            "example-org/example-repo#19"
        );
    }

    #[test]
    fn verified_pr_from_receipt_rejects_unaccepted_cross_repo_and_non_pr_inputs() {
        let origin = origin();
        let issue = "example-org/example-repo#7";

        // Unaccepted states and dry-run receipts.
        for state in ["pending", "running", "failed", "rejected", "pr-opened", ""] {
            let mut r = accepted_receipt();
            r.set("state", string(state)).unwrap();
            assert!(
                verified_pr_from_receipt(&r, issue, &origin).is_none(),
                "state={state}"
            );
        }
        let mut dry = accepted_receipt();
        dry.set("dry_run", Value::Bool(true)).unwrap();
        assert!(verified_pr_from_receipt(&dry, issue, &origin).is_none());

        for merge_state in ["SKIPPED", "requested", "UNKNOWN", "OPEN", "CLOSED"] {
            let mut r = accepted_receipt();
            r.set("merge", object([("state", string(merge_state))]))
                .unwrap();
            assert!(
                verified_pr_from_receipt(&r, issue, &origin).is_none(),
                "merge.state={merge_state}"
            );
        }

        // Cross-repo / mismatched target repo.
        assert!(
            verified_pr_from_receipt(&accepted_receipt(), "other-org/example-repo#7", &origin)
                .is_none()
        );
        let mut foreign_target = accepted_receipt();
        foreign_target
            .set("target_repo", string("other-org/example-repo"))
            .unwrap();
        assert!(verified_pr_from_receipt(&foreign_target, issue, &origin).is_none());

        // Non-PR URLs, cross-repo URLs, spoofed hosts, trailing junk, missing URL.
        for bad_url in [
            "",
            "https://github.com/other-org/example-repo/pull/19",
            "https://github.com/example-org/other-repo/pull/19",
            "https://github.com/example-org/example-repo/issues/19",
            "https://github.com/example-org/example-repo/pull/19/files",
            "https://github.com/example-org/example-repo/pull/19?w=1",
            "https://github.com/example-org/example-repo/pull/19#issuecomment-1",
            "https://github.com/example-org/example-repo/pull/20",
            "http://github.com/example-org/example-repo/pull/19",
            "https://evil.example/example-org/example-repo/pull/19",
            "https://github.com.evil.example/example-org/example-repo/pull/19",
            " https://github.com/example-org/example-repo/pull/19",
        ] {
            let mut r = accepted_receipt();
            r.set("pr_url", string(bad_url)).unwrap();
            assert!(
                verified_pr_from_receipt(&r, issue, &origin).is_none(),
                "pr_url={bad_url:?}"
            );
        }
        let mut no_url = accepted_receipt();
        no_url.remove("pr_url");
        assert!(verified_pr_from_receipt(&no_url, issue, &origin).is_none());

        let mut wrong_num = accepted_receipt();
        wrong_num.set("pr_number", number(20)).unwrap();
        assert!(verified_pr_from_receipt(&wrong_num, issue, &origin).is_none());
    }
}
