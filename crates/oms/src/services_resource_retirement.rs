//! Native disposition producers. Store callbacks validate retained evidence and
//! locks only; canonical owner reads and Git checks surround the dispatch writer.
use super::{before, error, mutation_error, policy, store_error};
use oms_command_plan::{Plan, Result, json, storage::Store};
use oms_command_telemetry::store::{StoreError, dispatch, resources};
use oms_runtime::services::ResourceAction;
use oms_runtime::value::{Value, b, number, object, string};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(error(75, message)) }
}

fn retain(
    store: &Store,
    body: Value,
    revision: u64,
) -> Result<(resources::DecisionEvidence, Value)> {
    let reference = oms_command_plan::decision::retain_native(store, &body)?;
    Ok((
        resources::DecisionEvidence {
            record: reference.text("path").into(),
            sha256: reference.text("sha256").into(),
            revision: format!("resource-{revision}"),
        },
        reference,
    ))
}

fn team_spec(plan: &Plan<'_>, id: &str, digest: &str, withdrawn: bool) -> Result<Value> {
    let snapshot = plan.load()?;
    let record = snapshot.plan.get("teams").get(id);
    let versions = record.get("versions").array()?;
    let selected = versions
        .iter()
        .find(|version| version.text("sha256") == digest)
        .ok_or_else(|| {
            error(
                75,
                "resource Team generation is not retained by the canonical Plan",
            )
        })?;
    let superseded = versions.last() != Some(selected);
    require(
        !withdrawn || record.text("phase") == "settled" || superseded,
        "retire-team requires withdrawal or canonical supersession of this exact Team generation",
    )?;
    // Validate the entire retained contract chain through its canonical reader.
    // This read takes no new producer lock while the root guard is held.
    let request =
        oms_command_plan::Request::parse(&["team-show".into(), "--id".into(), id.into()])?;
    plan.execute(&request)?;
    let path = format!(".oms/plan/team-specs/{digest}.json");
    require(
        selected.text("path") == path,
        "resource Team reference is foreign",
    )?;
    let raw = plan
        .store
        .read(&path, oms_command_plan::team::MAX_SPEC_BYTES)?;
    require(json::sha(&raw) == digest, "resource Team contract changed")?;
    let spec = oms_command_plan::team::TeamSpec::parse(&raw)?.value;
    require(
        spec.text("id") == id
            && spec.text("root_owner") == plan.context.panel_main_attempt
            && spec.text("room_id") == plan.context.room_id,
        "resource Team belongs to another canonical owner",
    )?;
    Ok(spec)
}

pub(super) fn retire(
    plan: &Plan<'_>,
    action: &ResourceAction,
    host: &str,
    deadline: Instant,
) -> Result<Value> {
    // A root guard remains valid after Team withdrawal and task completion. A
    // Team admission guard cannot authorize settlement after its own expiry.
    let guard = plan.root_admission_before(deadline)?;
    let bounded = guard.writer_before()?;
    let plan = &bounded;
    let snapshot = resources::snapshot(&plan.context.state_repo, plan.interrupted)
        .map_err(store_error)?
        .ok_or_else(|| error(75, "resource domain is not configured"))?;
    require(
        snapshot.policy.host_generation_sha256 == host,
        "resource host changed; unproven old custody cannot be retired",
    )?;
    let owner = guard.binding().text("root_owner");
    let generation = guard.binding().text("root_owner_generation");
    let mut body = object([
        ("schema", number(1)),
        ("kind", string("oms-resource-disposition")),
        ("root_owner", string(owner)),
        ("root_generation", string(generation)),
        ("room_id", string(&plan.context.room_id)),
        ("host_generation_sha256", string(host)),
        ("artifacts_preserved", b(true)),
    ]);
    let (binding, revision, operation) = match action {
        ResourceAction::RetireTeam {
            team_id,
            spec_sha256,
            expected_revision,
        } => {
            let spec = team_spec(plan, team_id, spec_sha256, true)?;
            let slot = snapshot
                .teams
                .iter()
                .find(|team| team.id == *team_id && team.spec_sha256 == *spec_sha256)
                .ok_or_else(|| error(75, "resource Team slot is absent"))?;
            require(
                slot.root_owner == owner
                    && slot.root_generation == generation
                    && spec.text("root_owner_generation") == generation,
                "resource Team owner generation changed",
            )?;
            let member = spec
                .get("members")
                .array()?
                .first()
                .ok_or_else(|| error(75, "resource Team has no member"))?;
            body.set("disposition", string("closed"))?;
            body.set("team_id", string(team_id))?;
            body.set("team_spec_sha256", string(spec_sha256))?;
            body.set("expected_revision", number(*expected_revision))?;
            (
                resources::Binding {
                    policy_sha256: snapshot.policy.sha256().map_err(store_error)?,
                    host_generation_sha256: host.into(),
                    root_owner: owner.into(),
                    root_generation: generation.into(),
                    team: Some(resources::Team {
                        id: team_id.clone(),
                        spec_sha256: spec_sha256.clone(),
                        member: member.text("id").into(),
                        member_generation: member.get("generation").int()?,
                    }),
                    reviewer: false,
                    persistent_session: false,
                    cleanup_scopes: vec![
                        oms_supervisor::process::Scope::ProcessGroup,
                        oms_supervisor::process::Scope::WholeTree,
                    ],
                },
                *expected_revision,
                None,
            )
        }
        ResourceAction::DiscardWrite {
            operation_id,
            expected_revision,
            reason,
        } => {
            require(
                !reason.trim().is_empty()
                    && reason.chars().count() <= 300
                    && !reason.chars().any(char::is_control),
                "write disposition requires a bounded reason",
            )?;
            let write = snapshot
                .writes
                .iter()
                .find(|write| write.operation_id == *operation_id)
                .ok_or_else(|| error(75, "resource write operation is absent"))?;
            require(
                write.binding.root_owner == owner
                    && write.binding.root_generation == generation
                    && write.binding.host_generation_sha256 == host,
                "resource write belongs to another owner or host generation",
            )?;
            require(
                !write.execution_pending,
                "write cleanup is pending or unknown; preserve candidate and custody",
            )?;
            if let Some(team) = &write.binding.team {
                let spec = team_spec(plan, &team.id, &team.spec_sha256, false)?;
                require(
                    spec.text("root_owner_generation") == generation,
                    "write Team owner generation changed",
                )?;
            }
            body.set("disposition", string("discarded"))?;
            body.set("operation_id", string(operation_id))?;
            body.set("expected_revision", number(*expected_revision))?;
            body.set("reason", string(reason))?;
            body.set(
                "call_ids",
                Value::Array(write.call_ids.iter().map(string).collect()),
            )?;
            (
                write.binding.clone(),
                *expected_revision,
                Some(operation_id.as_str()),
            )
        }
        _ => return Err(error(2, "unsupported resource disposition")),
    };
    guard.check()?;
    let (decision, reference) = retain(&plan.store, body, revision)?;
    guard.check()?;
    let check = |repo: &Path, target: &resources::DecisionTarget<'_>| {
        guard
            .check_pinned()
            .map_err(|_| StoreError::Invalid("resource decision producer guard changed"))?;
        let matches = match target {
            resources::DecisionTarget::Team {
                binding: actual,
                revision: actual_revision,
            } => operation.is_none() && **actual == binding && *actual_revision == revision,
            resources::DecisionTarget::Write {
                binding: actual,
                operation_id,
                revision: actual_revision,
            } => {
                operation == Some(*operation_id)
                    && **actual == binding
                    && *actual_revision == revision
            }
            _ => false,
        };
        if repo != plan.context.state_repo || !matches {
            return Err(StoreError::Invalid("resource decision target changed"));
        }
        Ok(decision.clone())
    };
    if let Some(operation) = operation {
        resources::retire_write_checked(
            &plan.context.state_repo,
            &binding,
            operation,
            revision,
            plan.interrupted,
            deadline,
            check,
        )
        .map_err(mutation_error)?;
    } else {
        resources::retire_team_checked(
            &plan.context.state_repo,
            &binding,
            revision,
            plan.interrupted,
            deadline,
            check,
        )
        .map_err(mutation_error)?;
    }
    guard.check().map_err(|_| {
        error(
            75,
            "resource retirement may already be published; reconcile its decision reference",
        )
    })?;
    Ok(reference)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile(
    repo: &Path,
    state_repo: &Path,
    selected: &Path,
    expected: &str,
    reason: &str,
    host: &str,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Value> {
    require(
        !reason.trim().is_empty()
            && reason.chars().count() <= 300
            && !reason.chars().any(char::is_control),
        "epoch reconciliation requires a bounded reason",
    )?;
    let (next, captured) = policy(repo, selected)?;
    require(
        next.host_generation_sha256 == host,
        "next resource policy does not match the actual host",
    )?;
    let raw = dispatch::read_snapshot(state_repo, stop)
        .map_err(store_error)?
        .ok_or_else(|| error(75, "resource registry is absent"))?;
    require(
        json::sha(&raw) == expected,
        "resource epoch snapshot changed",
    )?;
    let snapshot = resources::snapshot(state_repo, stop)
        .map_err(store_error)?
        .ok_or_else(|| error(75, "resource domain is absent"))?;
    require(
        snapshot.sha256 == expected
            && !snapshot.active
            && snapshot.occupancy == resources::Occupancy::default(),
        "resource epoch requires the exact held and fully drained snapshot",
    )?;
    let previous = snapshot.policy.sha256().map_err(store_error)?;
    let next_digest = next.sha256().map_err(store_error)?;
    let body = object([
        ("schema", number(1)),
        ("kind", string("oms-resource-epoch-decision")),
        ("expected_snapshot_sha256", string(expected)),
        ("previous_policy_sha256", string(&previous)),
        ("next_policy_sha256", string(&next_digest)),
        ("host_generation_sha256", string(host)),
        ("reason", string(reason)),
    ]);
    let store = Store::open(state_repo)?;
    let (decision, reference) = retain(&store, body, 1)?;
    before(stop, deadline)?;
    resources::reconcile_checked(
        state_repo,
        &raw,
        &next,
        stop,
        deadline,
        |actual_repo, target| {
            let resources::DecisionTarget::PolicyEpoch {
                current,
                next: actual,
            } = target
            else {
                return Err(StoreError::Invalid("resource epoch target changed"));
            };
            if actual_repo != state_repo || current.sha256()? != previous || **actual != next {
                return Err(StoreError::Invalid("resource epoch decision changed"));
            }
            Ok(decision.clone())
        },
    )
    .map_err(mutation_error)?;
    if policy(repo, selected)?.1 != captured {
        return Err(error(
            75,
            "resource epoch may already be published; next policy source changed",
        ));
    }
    Ok(reference)
}
