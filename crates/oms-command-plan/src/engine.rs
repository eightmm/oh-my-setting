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
    pub(crate) fn resolve(
        req: &Request,
        default_repo: &Path,
        cwd: &Path,
        interrupted: &AtomicBool,
        hint: &dyn Fn(&Path) -> Result<Option<PathBuf>>,
    ) -> Result<Self> {
        use crate::cli::{physical_dir, physical_leaf, worktree_root};
        let get = |n: &str| std::env::var(n).unwrap_or_default();
        let repo = req.repo.as_deref().unwrap_or(default_repo);
        let execution_repo = worktree_root(&physical_dir(cwd, repo.as_os_str()), interrupted)?;
        let state = match std::env::var_os("OMS_STATE_REPO").filter(|s| !s.is_empty()) {
            Some(s) => worktree_root(&physical_dir(cwd, &s), interrupted)
                .map_err(|_| error("cannot resolve plan repository"))?,
            None => match hint(&execution_repo)? {
                Some(h) => crate::cli::canonicalize_dir(&h)
                    .map_err(|_| error("cannot resolve plan repository"))?,
                None => execution_repo.clone(),
            },
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
            repo: &self.context.execution_repo,
            interrupted: self.interrupted,
            group_resolver: self.services.group_membership_resolver(),
        }
    }
    pub fn state_git(&self) -> Git<'_> {
        Git {
            repo: &self.context.state_repo,
            interrupted: self.interrupted,
            group_resolver: self.services.group_membership_resolver(),
        }
    }
    pub fn load(&self) -> Result<Snapshot> {
        let snap = self
            .plan_storage()
            .snapshot(&self.context.plan_file, MAX_JSON, true)?;
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
    pub fn execute(&self, req: &Request) -> Result<Response> {
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
            return self.locked(false, Some(&self.context.room_id), || {
                crate::team::execute(self, req)
            });
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
                | Cancel
                | RelocateDecision
                | Amend
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
        if matches!(req.action, Cancel | RelocateDecision | Amend | Retire) {
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
                RecoverLease | RecoverOwner | Cancel | RelocateDecision | Amend
            ),
            None,
            || {
                if self.context.canonical() && !(req.action == RecoverLease && req.has("check")) {
                    crate::retirement::cleanup(self)?;
                }
                let mut s = self.load()?;
                self.mutate(req, &mut s)
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
        if !(t.text("state") == "claimed" || running && t.text("state") == "running") {
            return Ok(false);
        }
        let Some(age) = age(t, &self.context.ts) else {
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
    fn mutate(&self, req: &Request, s: &mut Snapshot) -> Result<Response> {
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
            ApplyProposal => return self.proposal(req, s),
            Amend => return self.amend(req, s),
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
        if matches!(req.action, Claim | Land | Finish) {
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
        if matches!(req.action, Claim | Start | Review | Repair | Land | Finish) {
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
            Start => {
                require(
                    state == "claimed",
                    format!("task {id} is {state}; claim it first"),
                )?;
                self.lease(req, &t)?;
                t.set("state", string("running"))?;
            }
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
                t.set("state", string("review"))?;
                evidence(req, &mut t)?;
                t.set("review_lease_id", t.get("lease_id").clone())?;
                t.set("executor_id", string(""))?;
                t.set("executor_soul_sha256", string(""))?;
                t.set("repair_artifact", string(""))?;
                if let Some(record) = acceptance {
                    t.set("panel_accept", record)?;
                } else {
                    t.remove("panel_accept");
                }
            }
            Repair => {
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
                t.set("state", string("claimed"))?;
                t.set("claimed_at", string(&self.context.ts))?;
                t.set("reason", string(""))?;
                t.set("repair_count", number(1))?;
                if !req.text("artifact")?.is_empty() {
                    t.set("repair_artifact", string(req.text("artifact")?))?;
                }
            }
            Land => self.land(req, &mut t)?,
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
    fn land(&self, req: &Request, t: &mut Value) -> Result<()> {
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
        t.set("state", string("landing"))
    }
    fn finish(&self, req: &Request, s: &mut Snapshot, t: &mut Value) -> Result<bool> {
        self.lease(req, t)?;
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
            evidence(req, t)?;
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
        let expected = req.value("expected-landing-receipt-sha256")?;
        require(
            json::hex(expected, 64),
            "--expected-landing-receipt-sha256 must be lowercase SHA-256",
        )?;
        require(
            json::receipt_digest(t)? == expected,
            "landing receipt changed; stale finish rejected",
        )?;
        if req.has("refreeze-acceptance") {
            self.refreeze(req, s, t, expected)?;
        }
        evidence(req, t)?;
        t.set("state", string("done"))?;
        Ok(false)
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
    fn panel_accept_record(
        &self,
        id: &str,
        t: &Value,
        plan: &Value,
        record: &Value,
        req: &Request,
        from_task: bool,
    ) -> Result<()> {
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
            &[],
        )?;
        require(
            record.get("schema").is_int(1)
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
                && gate.get("gate_verify_exit").is_int(0)
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
    Ok(())
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
