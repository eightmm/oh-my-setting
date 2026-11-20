//! Read adapters retain the owning domains' validation; projections grant no leases.
use crate::{Context, Error, Result, V, storage};
use oms_command_plan::{Plan, Request, services::ProjectedLifecycle, services::ProjectedRoom};
use std::path::{Path, PathBuf};

fn domain(error: impl std::fmt::Display) -> oms_command_plan::Error {
    oms_command_plan::Error {
        code: 2,
        message: error.to_string(),
    }
}
pub(crate) fn state_repo(repo: &Path) -> Result<PathBuf> {
    oms_command_room::Services::state_repository(&oms_command_room::LocalServices, repo)
        .map_err(|e| Error::new(2, e.to_string()))
}
pub(crate) struct Services<'a, 'b>(pub &'a Context<'b>);
impl oms_command_plan::Services for Services<'_, '_> {
    fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        self.0.executor.group_resolver()
    }
    fn project_lifecycle(&self, raw: &[u8]) -> oms_command_plan::Result<ProjectedLifecycle> {
        let rows = storage::rows(raw).map_err(domain)?;
        let attempts = oms_command_operations::project_attempts(&rows).map_err(domain)?;
        let attempts = V::Obj(
            attempts
                .into_iter()
                .map(|row| (crate::text(&row, "attempt_id").as_bytes().to_vec(), row))
                .collect(),
        );
        Ok(ProjectedLifecycle {
            source_sha256: storage::hash(raw),
            attempts: oms_command_plan::json::parse(&attempts.json(false).map_err(domain)?)?,
        })
    }
    fn project_room(&self, _: &str, raw: &[u8]) -> oms_command_plan::Result<ProjectedRoom> {
        let rows = storage::rows(raw)
            .map_err(domain)?
            .iter()
            .map(|row| {
                oms_command_room::value::parse(&row.json(false).map_err(domain)?, storage::MAX_ROW)
                    .map_err(domain)
            })
            .collect::<oms_command_plan::Result<Vec<_>>>()?;
        let state = oms_command_room::project(&rows).map_err(domain)?;
        let raw_state = oms_command_room::value::encode(&state, false, true).map_err(domain)?;
        Ok(ProjectedRoom {
            source_sha256: storage::hash(raw),
            state: oms_command_plan::json::parse(&raw_state)?,
        })
    }
    fn normalize_provider(&self, provider: &str) -> oms_command_plan::Result<String> {
        crate::providers::normalize(provider).map_err(domain)
    }
}
impl oms_command_autonomy::Services for Services<'_, '_> {
    fn invoke(
        &self,
        command: &oms_command_autonomy::Invocation,
        interrupted: &std::sync::atomic::AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::CommandResult> {
        use crate::process::HandoffProjection;
        let unavailable = || {
            oms_command_autonomy::Error::new(2, "native handoff projection unavailable")
        };
        let state = command
            .environment
            .iter()
            .find_map(|(key, value)| (key == "OMS_STATE_REPO").then_some(Path::new(value)))
            .ok_or_else(unavailable)?;
        let projection = [
            HandoffProjection::TaskStatus,
            HandoffProjection::EvidenceMatrix,
            HandoffProjection::NextStep,
            HandoffProjection::JournalDecisions,
            HandoffProjection::ReviewFindings,
        ]
        .into_iter()
        .find(|projection| {
            let fixed = projection.command(&command.cwd, state, command.wall);
            fixed.argv[1] == command.tool.as_str() && fixed.argv[2..] == command.args
        })
        .ok_or_else(unavailable)?;
        if command.wall.is_zero() {
            return Err(unavailable());
        }
        let completed = self
            .0
            .executor
            .handoff_projection(projection, &command.cwd, state, command.wall, interrupted)
            .map_err(|e| oms_command_autonomy::Error::new(e.code, e.message))?;
        if !completed.complete
            || completed.stdout.len().saturating_add(completed.stderr.len()) > 65536
        {
            return Err(unavailable());
        }
        Ok((completed.code, completed.stdout, completed.stderr))
    }
}
impl Services<'_, '_> {
    pub(crate) fn plan(&self, repo: &Path, state: &Path) -> Result<Plan<'_>> {
        let req = Request::new(oms_command_plan::Action::Status);
        let mut context =
            oms_command_plan::Context::from_environment(&req, repo).map_err(domain_peer)?;
        context.state_repo = state.to_owned();
        context.plan_file = state.join(".oms/plan/tasks.json");
        Plan::open(context, self, self.0.interrupted).map_err(domain_peer)
    }
}
fn domain_peer(error: oms_command_plan::Error) -> Error {
    Error::new(error.code, error.message)
}
struct TaskRead(PathBuf);
impl oms_command_task::Services for TaskRead {
    fn state_repository(
        &mut self,
        _: &Path,
        _: &std::sync::atomic::AtomicBool,
    ) -> oms_command_task::Result<PathBuf> {
        Ok(self.0.clone())
    }
    fn begin_reference(
        &mut self,
        _: &Path,
        _: &Path,
    ) -> oms_command_task::Result<Box<dyn oms_command_task::services::ReferenceOperation>> {
        Err(oms_command_task::Error::service(
            "peer read adapter cannot mutate tasks",
        ))
    }
    fn observe_journal(
        &mut self,
        _: oms_command_task::services::JournalObservation<'_>,
    ) -> oms_command_task::Result<()> {
        Err(oms_command_task::Error::service(
            "peer read adapter cannot mutate journals",
        ))
    }
    fn promote_memory(
        &mut self,
        _: oms_command_task::services::MemoryOutcome<'_>,
    ) -> oms_command_task::Result<()> {
        Err(oms_command_task::Error::service(
            "peer read adapter cannot mutate memory",
        ))
    }
    fn observe_failure(
        &mut self,
        _: oms_command_task::services::FailureObservation<'_>,
    ) -> oms_command_task::Result<Vec<u8>> {
        Err(oms_command_task::Error::service(
            "peer read adapter cannot mutate failures",
        ))
    }
    fn observe_verify(
        &mut self,
        _: &Path,
        _: &[u8],
    ) -> oms_command_task::Result<oms_command_task::services::VerifyObservation> {
        Err(oms_command_task::Error::service(
            "peer read adapter cannot verify tasks",
        ))
    }
    fn skill_contracts(&mut self, _: &Path) -> oms_command_task::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Err(oms_command_task::Error::service("skill contracts"))
    }
}
impl oms_runtime::Services for Services<'_, '_> {
    fn context_dependencies(
        &self,
        repo: &Path,
        targets: &[PathBuf],
    ) -> oms_command_plan::Result<Vec<oms_runtime::services::RelatedSource>> {
        self.0
            .executor
            .context_dependencies(repo, targets, self.0.interrupted)
    }
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        self.0.executor.group_resolver()
    }
    fn repository_view(
        &self,
        repo: &Path,
    ) -> oms_command_plan::Result<oms_runtime::services::RepositoryView> {
        let head = crate::process::git(self.0, repo, &["rev-parse", "HEAD"]).map_err(domain)?;
        let branch = crate::process::git(self.0, repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map_err(domain)?;
        let worktree_state =
            oms_command_task::state_fingerprint(repo, self.0.interrupted).map_err(domain)?;
        Ok(oms_runtime::services::RepositoryView {
            head: String::from_utf8_lossy(&head).trim().into(),
            branch: String::from_utf8_lossy(&branch).trim().into(),
            worktree_state,
        })
    }
    fn task_status(
        &self,
        repo: &Path,
        state: &Path,
        packet: &[u8],
    ) -> oms_command_plan::Result<Vec<u8>> {
        let path = Path::new(".oms/task/current.md");
        if storage::read(state, path, false).map_err(domain)? != packet {
            return Err(domain("task changed during projection"));
        }
        let (code, stdout, _) = oms_command_task::run_with_services(
            &["status".into(), "--json".into()],
            repo,
            self.0.interrupted,
            &mut TaskRead(state.to_owned()),
        );
        if code != 0 {
            return Err(domain("native task status failed"));
        }
        if storage::read(state, path, false).map_err(domain)? != packet {
            return Err(domain("task changed during projection"));
        }
        oms_work_inspect::task_status_snapshot(&stdout).map_err(domain)?;
        Ok(stdout)
    }
    fn plan_status(
        &self,
        repo: &Path,
        state: &Path,
        raw: &[u8],
    ) -> oms_command_plan::Result<Vec<u8>> {
        let plan = self.plan(repo, state).map_err(domain)?;
        let snapshot: oms_command_plan::Snapshot = plan.load()?;
        if snapshot.raw.as_deref() != Some(raw) {
            return Err(domain("plan changed during projection"));
        }
        let status = plan.status_snapshot()?;
        if plan.load()?.raw.as_deref() != Some(raw) {
            return Err(domain("plan changed during projection"));
        }
        oms_command_plan::json::canonical(&status)
    }
}
pub(crate) fn criteria(repo: &Path, context: &Context<'_>) -> Result<V> {
    let services = Services(context);
    let state = state_repo(repo)?;
    let mut runtime = oms_runtime::storage::Runtime::open(
        repo,
        &state,
        context.source,
        &services,
        context.interrupted,
    )
    .map_err(domain_peer)?;
    runtime.cwd = context.cwd.to_owned();
    let project = oms_runtime::projection::project(&runtime).map_err(domain_peer)?;
    let task = oms_runtime::projection::task(&runtime).map_err(domain_peer)?;
    let plan = oms_runtime::projection::plan(&runtime).map_err(domain_peer)?;
    let criteria =
        oms_runtime::projection::criteria(&project, &task, &plan).map_err(domain_peer)?;
    let criteria = V::parse(&oms_runtime::value::canonical(&criteria).map_err(domain_peer)?)?;
    let mut dependencies = crate::obj([]);
    for layer in [&project, &task, &plan] {
        let source = layer.get("source");
        if !source.text("path").is_empty() && !source.text("sha256").is_empty() {
            dependencies.set(source.text("path"), source.text("sha256"));
        }
    }
    Ok(crate::obj([
        ("criteria", criteria),
        ("dependency_digests", dependencies),
        ("active_task_id", task.text("task_id").into()),
        ("plan_id", plan.text("plan_id").into()),
    ]))
}
