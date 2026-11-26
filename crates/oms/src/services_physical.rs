//! Canonical parent qualification around the existing physical producer.
use oms_command_peer::process::{PhysicalAttempt, PhysicalContract, RegisterPhysical};
use oms_command_peer::{Error, Result};
use oms_command_plan::{Action, Context, Plan, Request, json, repository::Git};
use oms_command_telemetry::store::resources::{self, Binding, Policy, Team};
use oms_supervisor::process::Scope;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

fn before(stop: &AtomicBool, deadline: Instant) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err(Error::new(130, "physical contract admission interrupted"));
    }
    if Instant::now() >= deadline {
        return Err(Error::new(
            75,
            "physical contract admission deadline expired",
        ));
    }
    Ok(())
}
fn plan_error(error: oms_command_plan::Error) -> Error {
    Error::new(error.code, error.message)
}
pub(super) fn physical_domain_before(
    state: &Path,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<Option<resources::Summary>> {
    before(stop, deadline)?;
    let Some(domain) = resources::summary(state, stop)
        .map_err(|error| Error::new(error.exit_code(), error.to_string()))?
    else {
        return Ok(None);
    };
    if !domain.active {
        return Err(Error::new(75, "resource domain is held"));
    }
    let host = crate::services_runtime::observed_host_generation_before(deadline, stop)
        .map_err(plan_error)?;
    if host != domain.policy.host_generation_sha256 {
        return Err(Error::new(
            75,
            "resource host generation changed; reconcile the epoch",
        ));
    }
    before(stop, deadline)?;
    Ok(Some(domain))
}

impl super::CollabServices<'_> {
    pub(super) fn physical_contract(
        &self,
        attempt: &PhysicalAttempt<'_>,
        deadline: Instant,
        stop: &AtomicBool,
        register: RegisterPhysical<'_>,
    ) -> Result<()> {
        let Some(domain) = physical_domain_before(attempt.state_root, deadline, stop)? else {
            if attempt.team.is_some() {
                return Err(Error::new(
                    75,
                    "Team launch requires an explicit active resource domain",
                ));
            }
            return register(PhysicalContract::default()).map(|_| ());
        };
        let host = &domain.policy.host_generation_sha256;
        let cwd = std::env::current_dir()?;
        let mut context = Context::from_environment_before(
            &Request::new(Action::Status),
            attempt.contract_repo,
            &cwd,
            Some(attempt.state_root),
            stop,
            deadline,
        )
        .map_err(plan_error)?;
        if context.state_repo != std::fs::canonicalize(attempt.state_root)? {
            return Err(Error::new(
                2,
                "contract state differs from physical accounting state",
            ));
        }
        if let Some(root) = attempt.root {
            // Native Main just created and joined this locator. The guard
            // proves its creation generation from canonical lifecycle/Room;
            // inherited child/depth guards remain untouched.
            context.panel_main_attempt = root.attempt.into();
            context.room_id = root.room_id.into();
            context.participant = root.participant.into();
        }
        let plan = Plan::open(context, &self.control, stop).map_err(plan_error)?;
        let guard = match attempt.team {
            Some(team) => plan.team_admission_before(
                oms_command_plan::team::AdmissionRequest {
                    team_id: team.team_id.clone(),
                    spec_sha256: team.spec_sha256.clone(),
                    member_id: team.member_id.clone(),
                    member_generation: team.member_generation,
                    provider: attempt.provider.into(),
                    harness: attempt.provider.into(),
                    host: "local".into(),
                    transport: "direct".into(),
                    account: attempt.account.map(str::to_owned),
                    selected_model: attempt.selected_model.map(str::to_owned),
                    selected_effort: attempt.selected_effort.map(str::to_owned),
                    write: attempt.write,
                },
                deadline,
            ),
            None => plan.root_admission_before(deadline),
        }
        .map_err(plan_error)?;
        let contract = guard.binding();
        if attempt.team.is_some() {
            if attempt.task_id != Some(contract.text("task_id")) {
                return Err(Error::new(
                    75,
                    "Team call task differs from its canonical contract",
                ));
            }
            if contract.text("mode") == "native-team"
                || !contract
                    .get("required_capabilities")
                    .array()
                    .map_err(plan_error)?
                    .is_empty()
                || !contract
                    .get("resources")
                    .array()
                    .map_err(plan_error)?
                    .is_empty()
                || contract.get("budget_ref") != &json::Value::Null
            {
                return Err(Error::new(
                    75,
                    "Team requires an unsupported native capability or resource binding",
                ));
            }
            let root = oms_safe_fs::Root::open(attempt.contract_repo)
                .map_err(|_| Error::new(2, "Team policy root unavailable"))?;
            let raw = root
                .read_no_follow(
                    Path::new(contract.text("policy_ref")),
                    oms_safe_fs::ByteReadOptions {
                        missing_ok: false,
                        max_bytes: oms_command_plan::team::MAX_SPEC_BYTES,
                    },
                )
                .map_err(|_| Error::new(2, "Team policy unavailable"))?;
            let policy =
                Policy::parse(&raw).map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
            if json::sha(&raw) != contract.text("policy_sha256")
                || policy
                    .sha256()
                    .map_err(|e| Error::new(e.exit_code(), e.to_string()))?
                    != domain
                        .policy
                        .sha256()
                        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?
            {
                return Err(Error::new(
                    75,
                    "Team policy differs from the active resource domain",
                ));
            }
        }
        let binding = Binding {
            policy_sha256: domain
                .policy
                .sha256()
                .map_err(|e| Error::new(e.exit_code(), e.to_string()))?,
            host_generation_sha256: host.clone(),
            root_owner: contract.text("root_owner").into(),
            root_generation: contract.text("root_owner_generation").into(),
            team: attempt.team.map(|team| Team {
                id: team.team_id.clone(),
                spec_sha256: team.spec_sha256.clone(),
                member: team.member_id.clone(),
                member_generation: team.member_generation,
            }),
            reviewer: attempt.team.is_some()
                && ["reviewer", "verifier"].contains(&contract.get("member").text("role")),
            persistent_session: attempt.persistent_session,
            cleanup_scopes: vec![Scope::ProcessGroup, Scope::WholeTree],
        };
        let same_execution_repo = std::fs::canonicalize(attempt.execution_repo)
            .ok()
            .zip(std::fs::canonicalize(attempt.contract_repo).ok())
            .is_some_and(|(execution, contract)| execution == contract);
        let check_execution = || -> Result<()> {
            before(stop, deadline)?;
            if same_execution_repo {
                return Ok(());
            }
            let git = Git {
                repo: attempt.execution_repo,
                interrupted: stop,
                group_resolver: None,
                deadline: Some(deadline),
                group_snapshot: None,
            };
            if git.text(&["rev-parse", "HEAD"]).map_err(plan_error)? != contract.text("base_sha") {
                return Err(Error::new(
                    75,
                    "physical execution base differs from the canonical owner",
                ));
            }
            before(stop, deadline)
        };
        check_execution()?;
        if attempt.team.is_some() {
            resources::reserve_team(attempt.state_root, &binding, stop, deadline)
                .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        }
        let call = register(PhysicalContract {
            resource: Some(&binding),
            expires_at: guard.expires_at_unix().map_err(plan_error)?,
        })?;
        guard.check_pinned().map_err(plan_error)?;
        if crate::services_runtime::observed_host_generation_before(deadline, stop)
            .map_err(plan_error)?
            != *host
        {
            return Err(Error::new(75, "physical host generation changed"));
        }
        resources::fence(attempt.state_root, &binding, &call, stop, deadline)
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        // Git, boot and resource checks may wait after registration. Recheck
        // the owner, policy and expiry after those last fallible fences.
        guard.check().map_err(plan_error)?;
        check_execution()?;
        before(stop, deadline)
    }
}
