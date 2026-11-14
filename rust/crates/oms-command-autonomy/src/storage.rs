use crate::{Error, Result, Services, checkpoint, require, value};
use oms_command_plan::storage::Store;
use oms_command_plan::{Action, Context as PlanContext, Plan, Request};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const MAX_FILE: usize = 1024 * 1024;
pub const MAX_LEDGER: usize = 16 * 1024 * 1024;
pub const ACTIVE: &str = ".oms/plan/tasks.json";
pub const PROGRESS: &str = ".oms/plan/progress.jsonl";
pub const RECEIPT: &str = ".oms/plan/autopilot-run.json";

pub(crate) struct Context<'a> {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub source: Store,
    pub store: Store,
    pub services: &'a dyn Services,
    pub interrupted: &'a AtomicBool,
    pub owner_id: String,
    pub run_id: String,
    pub cycle: Option<u64>,
    pub peer_wall: Option<std::time::Duration>,
}
impl<'a> Context<'a> {
    pub fn new(
        repo: PathBuf,
        state: PathBuf,
        services: &'a dyn Services,
        interrupted: &'a AtomicBool,
    ) -> Result<Self> {
        let groups = services.group_membership_resolver();
        Ok(Self {
            source: Store::open(&repo)?.with_group_resolver(groups),
            store: Store::open(&state)?.with_group_resolver(groups),
            repo,
            state,
            services,
            interrupted,
            owner_id: String::new(),
            run_id: String::new(),
            cycle: None,
            peer_wall: None,
        })
    }
    pub fn parent(&self) -> Result<()> {
        checkpoint(self.interrupted)?;
        let args = vec!["init".into()];
        oms_command_task::consumers::parent_guard(&args)
            .map_err(|e| Error::new(e.code, e.to_string()))
    }
    pub fn plan(&self) -> Result<Plan<'_>> {
        let mut req = Request::new(Action::Show);
        req.repo = Some(self.repo.clone());
        let mut context = PlanContext::from_environment(&req, &self.repo)?;
        context.execution_repo = self.repo.clone();
        context.state_repo = self.state.clone();
        context.plan_file = self.state.join(ACTIVE);
        if !self.owner_id.is_empty() {
            context.owner_id = self.owner_id.clone();
        }
        if !self.run_id.is_empty() {
            context.run_id = self.run_id.clone();
            context.cycle = self.cycle;
        }
        Ok(Plan::open(context, self.services, self.interrupted)?)
    }
    pub fn action(&self, req: &Request) -> Result<crate::Output> {
        checkpoint(self.interrupted)?;
        let out = self.plan()?.execute(req)?;
        Ok(crate::Output {
            code: out.code,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
    pub fn plan_value(&self) -> Result<Value> {
        value::from_plan(&self.plan()?.load()?.plan)
    }
    pub fn git(&self, args: &[&str]) -> Result<String> {
        Ok(self.plan()?.git().text(args)?)
    }
    pub fn bytes(&self, path: &Path, limit: usize) -> Result<Vec<u8>> {
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.repo.join(path)
        };
        if full.starts_with(&self.repo) {
            return Ok(self.source.read(full, limit)?);
        }
        if full.starts_with(&self.state) {
            return Ok(self.store.read(full, limit)?);
        }
        let parent = full
            .parent()
            .ok_or_else(|| Error::new(2, "file has no parent"))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| Error::new(2, "cannot resolve file parent"))?;
        let name = full
            .file_name()
            .ok_or_else(|| Error::new(2, "file has no name"))?;
        let root =
            oms_safe_fs::Root::open(parent).map_err(|_| Error::new(2, "unsafe file parent"))?;
        root.read_path(Path::new(name), limit, oms_safe_fs::Policy::manifest())
            .map_err(|_| Error::new(2, "file must be owned, bounded and regular without links"))
    }
    pub fn rows(&self, path: &str) -> Result<Vec<Value>> {
        let raw = self.store.optional(path, MAX_LEDGER)?.unwrap_or_default();
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        require(
            raw.ends_with(b"\n"),
            "incomplete ledger row; reconcile before retry",
        )?;
        raw.split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let v = value::parse(line)?;
                require(v.is_object(), "ledger row is not an object")?;
                Ok(v)
            })
            .collect()
    }
    pub fn append(&self, path: &str, v: &Value) -> Result<()> {
        // Store::append uses the existing native durable JSONL writer. Its
        // parent must exist; this harmless empty CAS creates only that parent.
        if self.store.optional(path, MAX_LEDGER)?.is_none() {
            self.store.publish_once(path, b"", 0)?;
        }
        self.store.append(path, &value::plan(v)?)?;
        Ok(())
    }
    pub fn lock(&self, path: &str) -> Result<oms_lock::Guard> {
        checkpoint(self.interrupted)?;
        let settings = oms_lock::Settings::from_env().map_err(|e| Error::new(2, e.to_string()))?;
        oms_lock::Lock::for_file(&self.state.join(path), settings)
            .and_then(|lock| lock.acquire(None))
            .map_err(|e| Error::new(2, e.to_string()))
    }
    pub fn clean(&self) -> Result<()> {
        self.plan()?.git().guard()?;
        require(
            self.git(&["status", "--porcelain", "--untracked-files=normal"])?
                .is_empty(),
            "working tree must be clean",
        )
    }
}
