//! Native policy administration for the physical producer's selected state root.
use oms_command_plan::{Error, Result};
use oms_command_telemetry::store::{self, dispatch, resources};
use oms_runtime::services::ResourceAction;
use oms_runtime::value::{self, Value, b, number, object, string};
use oms_safe_fs::Root;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SCOPE: &str = "repository-host-local-observed";
const POLICY_BYTES: usize = 64 * 1024;
#[path = "services_resource_retirement.rs"]
mod retirement;

fn error(code: i32, message: impl ToString) -> Error {
    Error {
        code,
        message: message.to_string(),
    }
}
fn store_error(source: store::StoreError) -> Error {
    error(source.exit_code(), source)
}
fn mutation_error(source: dispatch::Error) -> Error {
    if source.publication_uncertain {
        error(
            75,
            format!("resource update may already be published; reconcile current state: {source}"),
        )
    } else {
        error(source.exit_code(), source)
    }
}
fn before(stop: &AtomicBool, deadline: Instant) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err(error(130, "resource action interrupted"));
    }
    if Instant::now() >= deadline {
        return Err(error(75, "resource action deadline expired"));
    }
    Ok(())
}
fn parent() -> Result<()> {
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|value| value == "1")
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
            .is_some_and(|value| !value.is_empty() && value != "0")
    {
        return Err(error(2, "a harness child cannot mutate resource policy"));
    }
    Ok(())
}
fn policy(repo: &Path, selected: &Path) -> Result<(resources::Policy, Vec<u8>)> {
    let path = store::canonical_index(repo, Some(selected), false, false).map_err(store_error)?;
    let root = Root::open(repo).map_err(|_| error(2, "resource policy root unavailable"))?;
    let raw = root.read_manifest(&path, POLICY_BYTES)
        .map_err(|_| error(2, "resource policy must be an owned regular file inside the execution repository, at most 64 KiB"))?;
    Ok((resources::Policy::parse(&raw).map_err(store_error)?, raw))
}
fn status(state_repo: &Path, host: &str, stop: &AtomicBool) -> Result<Value> {
    let snapshot = resources::snapshot(state_repo, stop).map_err(store_error)?;
    let mut report = object([
        ("schema", number(1)),
        ("scope", string(SCOPE)),
        ("host_generation_sha256", string(host)),
        ("configured", b(snapshot.is_some())),
        ("active", b(false)),
        ("host_matches_policy", Value::Null),
        ("policy_sha256", Value::Null),
        ("policy", Value::Null),
        ("occupancy", Value::Null),
        ("snapshot_sha256", Value::Null),
        ("teams", Value::Array(vec![])),
        ("writes", Value::Array(vec![])),
    ]);
    if let Some(snapshot) = snapshot {
        report.set("snapshot_sha256", string(&snapshot.sha256))?;
        report.set(
            "teams",
            Value::Array(
                snapshot
                    .teams
                    .iter()
                    .map(|team| {
                        object([
                            ("id", string(&team.id)),
                            ("spec_sha256", string(&team.spec_sha256)),
                            ("root_owner", string(&team.root_owner)),
                            ("root_generation", string(&team.root_generation)),
                            ("revision", number(team.revision)),
                            ("active", b(team.active)),
                            ("execution_pending", b(team.execution_pending)),
                            ("candidate_pending", b(team.candidate_pending)),
                        ])
                    })
                    .collect(),
            ),
        )?;
        report.set(
            "writes",
            Value::Array(
                snapshot
                    .writes
                    .iter()
                    .map(|write| {
                        object([
                            ("operation_id", string(&write.operation_id)),
                            ("root_owner", string(&write.binding.root_owner)),
                            ("revision", number(write.revision)),
                            ("active", b(write.active)),
                            ("execution_pending", b(write.execution_pending)),
                            (
                                "call_ids",
                                Value::Array(write.call_ids.iter().map(string).collect()),
                            ),
                        ])
                    })
                    .collect(),
            ),
        )?;
        report.set("active", b(snapshot.active))?;
        report.set(
            "host_matches_policy",
            b(snapshot.policy.host_generation_sha256 == host),
        )?;
        report.set(
            "policy_sha256",
            string(snapshot.policy.sha256().map_err(store_error)?),
        )?;
        report.set(
            "policy",
            value::parse(&snapshot.policy.to_json().map_err(store_error)?)?,
        )?;
        report.set(
            "occupancy",
            object([
                ("teams", number(snapshot.occupancy.teams)),
                ("invocations", number(snapshot.occupancy.invocations)),
                ("open_processes", number(snapshot.occupancy.open_processes)),
                ("write_wip", number(snapshot.occupancy.write_wip)),
            ]),
        )?;
    }
    Ok(report)
}

pub(super) fn action(
    repo: &Path,
    state_repo: &Path,
    action: &ResourceAction,
    stop: &AtomicBool,
    control: &dyn oms_command_plan::Services,
) -> Result<Value> {
    // Root resolution occurs before this producer. This deadline bounds the
    // actual boot proof and mutation writer, not the whole public command.
    let deadline = Instant::now() + Duration::from_secs(10);
    before(stop, deadline)?;
    let mutation = !matches!(action, ResourceAction::Host | ResourceAction::Status);
    if mutation {
        parent()?;
    }
    let host = super::observed_host_generation_before(deadline, stop)?;
    let mut decision = None;
    let mut team_retirement = None;
    match action {
        ResourceAction::Host => {
            return Ok(object([
                ("schema", number(1)),
                ("scope", string(SCOPE)),
                ("host_generation_sha256", string(host)),
            ]));
        }
        ResourceAction::Status => {}
        ResourceAction::Activate { policy: selected } => {
            let (policy, captured) = policy(repo, selected)?;
            if policy.host_generation_sha256 != host {
                return Err(error(
                    75,
                    "resource policy does not match the actual host boot generation",
                ));
            }
            let expected = dispatch::read_snapshot(state_repo, stop).map_err(store_error)?;
            before(stop, deadline)?;
            resources::activate(state_repo, &policy, expected.as_deref(), stop, deadline)
                .map_err(mutation_error)?;
            let after = self::policy(repo, selected).map_err(|_| {
                error(
                    75,
                    "resource policy unavailable after activation; reconcile the published domain",
                )
            })?;
            if after.1 != captured {
                return Err(error(
                    75,
                    "resource policy changed after activation; reconcile the published domain",
                ));
            }
        }
        ResourceAction::Hold {
            expected_policy_sha256,
        } => {
            resources::hold(state_repo, expected_policy_sha256, stop, deadline)
                .map_err(mutation_error)?;
        }
        ResourceAction::Reconcile {
            policy: selected,
            expected_snapshot_sha256,
            reason,
        } => {
            decision = Some(retirement::reconcile(
                repo,
                state_repo,
                selected,
                expected_snapshot_sha256,
                reason,
                &host,
                stop,
                deadline,
            )?);
        }
        ResourceAction::RetireTeam { .. } | ResourceAction::DiscardWrite { .. } => {
            let context = oms_command_plan::Context::from_selected_state_before(
                &oms_command_plan::Request::new(oms_command_plan::Action::Status),
                repo,
                repo,
                state_repo,
                stop,
                deadline,
            )?;
            let plan = oms_command_plan::Plan::open(context, control, stop)?;
            decision = Some(retirement::retire(&plan, action, &host, deadline)?);
            if let ResourceAction::DiscardWrite { operation_id, .. } = action {
                let snapshot = resources::snapshot(state_repo, stop).map_err(store_error)?;
                if let Some(team) = snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .writes
                            .iter()
                            .find(|write| write.operation_id == *operation_id)
                    })
                    .and_then(|write| write.binding.team.as_ref())
                {
                    team_retirement = Some(
                        match team_withdrawn_before(
                            &plan.context,
                            &team.id,
                            &team.spec_sha256,
                            control,
                            stop,
                            deadline,
                        ) {
                            Ok(Some(value)) => value,
                            Ok(None) => object([("status", string("not_reserved"))]),
                            Err(error) => object([
                                ("status", string("unknown")),
                                ("reason", string(error.message)),
                            ]),
                        },
                    );
                }
            }
        }
    }
    before(stop, deadline)?;
    if super::observed_host_generation_before(deadline, stop)? != host {
        return Err(error(
            75,
            "resource host generation changed; reconcile current custody",
        ));
    }
    let mut report = status(state_repo, &host, stop)?;
    if let Some(decision) = decision {
        report.set("decision", decision)?;
    }
    if let Some(retirement) = team_retirement {
        report.set("team_retirement", retirement)?;
    }
    before(stop, deadline)?;
    Ok(report)
}

/// A withdrawal stops admission. Only independently drained custody can be
/// retired here; the original process owner remains responsible for cleanup.
pub(crate) fn team_withdrawn(
    context: &oms_command_plan::Context,
    team_id: &str,
    spec_sha256: &str,
    control: &dyn oms_command_plan::Services,
    stop: &AtomicBool,
) -> Result<Option<Value>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    team_withdrawn_before(context, team_id, spec_sha256, control, stop, deadline)
}
fn team_withdrawn_before(
    context: &oms_command_plan::Context,
    team_id: &str,
    spec_sha256: &str,
    control: &dyn oms_command_plan::Services,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Value>> {
    parent()?;
    before(stop, deadline)?;
    let Some(snapshot) = resources::snapshot(&context.state_repo, stop).map_err(store_error)?
    else {
        return Ok(None);
    };
    let Some(slot) = snapshot
        .teams
        .iter()
        .find(|slot| slot.id == team_id && slot.spec_sha256 == spec_sha256)
    else {
        return Ok(None);
    };
    let plan = oms_command_plan::Plan::open(context.clone(), control, stop)?;
    // This is only a projection. The guarded retirement revalidates the complete
    // canonical contract before any mutation, with its bounded native read set.
    let current = value::parse(&plan.store.read(
        oms_command_plan::storage::ACTIVE,
        oms_command_plan::storage::MAX_JSON,
    )?)?;
    let record = current.get("teams").get(team_id);
    let versions = record.get("versions").array()?;
    let retained = versions
        .iter()
        .any(|version| version.text("sha256") == spec_sha256);
    let current = versions
        .last()
        .is_some_and(|version| version.text("sha256") == spec_sha256);
    if !retained || (current && record.text("phase") != "settled") {
        return Ok(Some(object([("status", string("contract_active"))])));
    }
    let mut result = object([
        ("team_id", string(team_id)),
        ("team_spec_sha256", string(spec_sha256)),
        ("execution_pending", b(slot.execution_pending)),
        ("candidate_pending", b(slot.candidate_pending)),
        (
            "status",
            string(if !slot.active { "retired" } else { "pending" }),
        ),
    ]);
    if slot.active && !slot.execution_pending && !slot.candidate_pending {
        let host = super::observed_host_generation_before(deadline, stop)?;
        let reference = retirement::retire(
            &plan,
            &ResourceAction::RetireTeam {
                team_id: team_id.into(),
                spec_sha256: spec_sha256.into(),
                expected_revision: slot.revision,
            },
            &host,
            deadline,
        )?;
        if super::observed_host_generation_before(deadline, stop)? != host {
            return Err(error(
                75,
                "resource retirement may already be published; host generation changed",
            ));
        }
        result.set("status", string("retired"))?;
        result.set("decision", reference)?;
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_resource_cli_pins_host_policy_and_shared_state_without_main() {
        const NAME: &str = "services_runtime::resources::tests::native_resource_cli_pins_host_policy_and_shared_state_without_main";
        if let Some(repo) = std::env::var_os("OMS_RESOURCE_FIXTURE_REPO") {
            let repo = PathBuf::from(repo);
            let stop = Arc::new(AtomicBool::new(false));
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let services =
                super::super::RuntimeServices::with_overrides(&stop, source, vec![], None).unwrap();
            let words: Vec<String> =
                serde_json::from_str(&std::env::var("OMS_RESOURCE_FIXTURE_ARGS").unwrap()).unwrap();
            let args = words.iter().map(OsString::from).collect::<Vec<_>>();
            let before = std::fs::read(repo.join(dispatch::REGISTRY)).unwrap();
            let result = oms_runtime::run(&args, &repo, source, &services, stop.as_ref());
            let expected: i32 = std::env::var("OMS_RESOURCE_FIXTURE_EXIT")
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                result.code,
                expected,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            if expected != 0 {
                assert_eq!(
                    before,
                    std::fs::read(repo.join(dispatch::REGISTRY)).unwrap()
                );
            } else {
                let report = value::parse(&result.stdout).unwrap();
                assert_eq!(report.get("occupancy").get("teams").int().unwrap(), 0);
                assert_eq!(report.get("occupancy").get("write_wip").int().unwrap(), 0);
                if words[1] == "discard-write" {
                    assert_eq!(report.get("team_retirement").text("status"), "retired");
                }
            }
            return;
        }
        let Some(case) = std::env::var_os("OMS_RESOURCE_TEST_CASE") else {
            for case in ["parent", "child", "depth"] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args(["--exact", NAME])
                    .env("OMS_RESOURCE_TEST_CASE", case)
                    .env_remove("OMS_HARNESS_CHILD")
                    .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
                    .env_remove("OMS_STATE_REPO");
                if case == "child" {
                    child.env("OMS_HARNESS_CHILD", "1");
                }
                if case == "depth" {
                    child.env("OMS_HARNESS_DELEGATE_DEPTH", "1");
                }
                assert!(child.status().unwrap().success(), "{case}");
            }
            return;
        };
        let mut root =
            Temp(std::env::temp_dir().join(format!("oms-resource-cli-{}", std::process::id())));
        std::fs::create_dir(&root.0).unwrap();
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        root.0 = std::fs::canonicalize(&root.0).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root.0)
                .status()
                .unwrap()
                .success()
        );
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let services =
            super::super::RuntimeServices::with_overrides(&flag, &root.0, vec![], None).unwrap();
        let run = |words: &[&str]| {
            let mut argv = vec![OsString::from("--repo"), root.0.as_os_str().to_owned()];
            argv.extend(words.iter().map(OsString::from));
            oms_runtime::run(&argv, &nested, &root.0, &services, flag.as_ref())
        };
        let host = run(&["resources", "host"]);
        assert_eq!(host.code, 0, "{}", String::from_utf8_lossy(&host.stderr));
        let host = value::parse(&host.stdout).unwrap();
        let status = run(&["resources", "status"]);
        assert_eq!(
            status.code,
            0,
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(
            !value::parse(&status.stdout)
                .unwrap()
                .get("configured")
                .truth()
        );
        assert!(
            !root.0.join(".oms").exists(),
            "observations create no state"
        );

        let policy = resources::Policy {
            domain: "fixture-domain".into(),
            epoch: "fixture-epoch".into(),
            host_generation_sha256: host.text("host_generation_sha256").into(),
            max_teams: 1,
            max_invocations: 1,
            max_open_processes: 1,
            max_write_wip: 1,
            reviewer_headroom: 0,
        };
        let selected = root.0.join("policy.json");
        std::fs::write(&selected, policy.to_json().unwrap()).unwrap();
        if case != "parent" {
            assert_eq!(
                run(&["resources", "activate", "--policy", "../policy.json"]).code,
                2
            );
            assert_eq!(
                run(&[
                    "resources",
                    "hold",
                    "--expected-policy-sha256",
                    &"a".repeat(64)
                ])
                .code,
                2
            );
            assert!(
                action(
                    &root.0,
                    &root.0,
                    &ResourceAction::Activate { policy: selected },
                    flag.as_ref(),
                    &services.control,
                )
                .is_err()
            );
            assert!(!root.0.join(dispatch::REGISTRY).exists());
            return;
        }

        let mut wrong = policy.clone();
        wrong.host_generation_sha256 = "0".repeat(64);
        std::fs::write(&selected, wrong.to_json().unwrap()).unwrap();
        assert_eq!(
            run(&["resources", "activate", "--policy", "../policy.json"]).code,
            75
        );

        assert!(!root.0.join(dispatch::REGISTRY).exists());
        std::fs::write(&selected, policy.to_json().unwrap()).unwrap();
        let link = root.0.join("linked.json");
        symlink(&selected, &link).unwrap();
        assert_eq!(
            run(&["resources", "activate", "--policy", "../linked.json"]).code,
            2
        );
        std::fs::remove_file(&link).unwrap();
        std::fs::hard_link(&selected, &link).unwrap();
        assert_eq!(
            run(&["resources", "activate", "--policy", "../policy.json"]).code,
            2
        );
        std::fs::remove_file(link).unwrap();

        let activated = run(&["resources", "activate", "--policy", "../policy.json"]);
        assert_eq!(
            activated.code,
            0,
            "{}",
            String::from_utf8_lossy(&activated.stderr)
        );
        let activated = value::parse(&activated.stdout).unwrap();
        assert!(activated.get("active").truth() && activated.get("host_matches_policy").truth());
        assert_eq!(activated.text("policy_sha256"), policy.sha256().unwrap());
        assert!(
            !root.0.join(".oms/plan/tasks.json").exists(),
            "activation requires no enrolled Main"
        );
        let initial = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
        assert_eq!(
            run(&[
                "resources",
                "hold",
                "--expected-policy-sha256",
                &"a".repeat(64)
            ])
            .code,
            2
        );
        assert_eq!(
            std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
            initial
        );
        assert_eq!(
            run(&["resources", "activate", "--policy", "../policy.json"]).code,
            0
        );
        assert_eq!(
            std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
            initial
        );
        let held = run(&[
            "resources",
            "hold",
            "--expected-policy-sha256",
            &policy.sha256().unwrap(),
        ]);
        assert_eq!(held.code, 0, "{}", String::from_utf8_lossy(&held.stderr));
        assert!(!value::parse(&held.stdout).unwrap().get("active").truth());
        assert_eq!(
            run(&["resources", "activate", "--policy", "../policy.json"]).code,
            75
        );

        let held = value::parse(&held.stdout).unwrap();
        let mut next = policy.clone();
        next.epoch = "resumed-epoch".into();
        std::fs::write(&selected, next.to_json().unwrap()).unwrap();
        let resume = |snapshot: &str| {
            run(&[
                "resources",
                "reconcile",
                "--policy",
                "../policy.json",
                "--expected-snapshot-sha256",
                snapshot,
                "--reason",
                "resume drained work",
            ])
        };
        assert_eq!(resume(&"a".repeat(64)).code, 75);
        let resumed = resume(held.text("snapshot_sha256"));
        assert_eq!(
            resumed.code,
            0,
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let resumed = value::parse(&resumed.stdout).unwrap();
        assert!(resumed.get("active").truth());
        assert_eq!(resumed.text("policy_sha256"), next.sha256().unwrap());
        assert!(!resumed.get("decision").text("sha256").is_empty());
        assert_eq!(
            resume(held.text("snapshot_sha256")).code,
            75,
            "stale resume cannot replay over a new epoch"
        );

        let shared = root.0.join("shared-state");
        std::fs::create_dir(&shared).unwrap();
        let activated = action(
            &root.0,
            &shared,
            &ResourceAction::Activate {
                policy: selected.clone(),
            },
            flag.as_ref(),
            &services.control,
        )
        .unwrap();
        assert!(activated.get("active").truth());
        assert!(
            shared.join(dispatch::REGISTRY).exists(),
            "selected physical accounting state is authoritative"
        );
        let occupied = root.0.join("legacy-state");
        std::fs::create_dir(&occupied).unwrap();
        oms_command_telemetry::store::physical::begin(
            &occupied,
            oms_command_telemetry::store::usage::Identity {
                call_id: "call_legacy".into(),
                operation_id: "operation_legacy".into(),
                provider: "codex".into(),
                ..Default::default()
            },
            None,
            false,
            flag.as_ref(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        let history = std::fs::read(occupied.join(dispatch::REGISTRY)).unwrap();
        assert!(
            action(
                &root.0,
                &occupied,
                &ResourceAction::Activate { policy: selected },
                flag.as_ref(),
                &services.control,
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(occupied.join(dispatch::REGISTRY)).unwrap(),
            history
        );
        retirement_flow(&root.0, &services, &flag, &next, NAME);
    }

    // Extends the policy fixture through the public resource CLI with a real
    // native Main/Room and sealed local-process cleanup, without provider calls.
    fn retirement_flow(
        repo: &Path,
        services: &super::super::RuntimeServices<'_>,
        stop: &Arc<AtomicBool>,
        policy: &resources::Policy,
        test_name: &str,
    ) {
        use oms_command_operations::value::obj;
        use oms_command_operations::{StartRequest, append_lifecycle, create_attempt, new_event};
        use oms_command_peer::process::{Executor, Invocation, Native};
        use oms_command_plan::{Action, Context, Plan, Request, json};
        use oms_command_telemetry::store::{physical, usage};
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        std::fs::create_dir(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/base"), b"base\n").unwrap();
        git(&["add", "src/base"]);
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "base",
        ]);
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let collab = crate::services_collab::CollabServices::new(stop, source).unwrap();
        let room = oms_command_room::Room::open(
            repo,
            oms_command_room::Session {
                agent: "codex".into(),
                ..Default::default()
            },
            &collab,
            stop,
            true,
            false,
        )
        .unwrap();
        room.create(
            Some("room-team"),
            "Resource retirement fixture",
            Some("task-a"),
            None,
            None,
        )
        .unwrap();
        let owner = create_attempt(
            repo,
            &StartRequest {
                provider: "codex".into(),
                tool: "panel-main".into(),
                refs: obj([
                    ("panel_room_id", "room-team".into()),
                    ("panel_role", "main".into()),
                ]),
                ..Default::default()
            },
            stop,
        )
        .unwrap();
        for state in ["starting", "working"] {
            append_lifecycle(
                repo,
                new_event(
                    &owner,
                    0usize.into(),
                    "attempt.state_changed",
                    obj([("to_state", state.into())]),
                )
                .unwrap(),
                stop,
            )
            .unwrap();
        }
        room.join("room-team", oms_command_room::Join::new(&owner, "codex"))
            .unwrap();
        let mut context = Context::from_selected_state_before(
            &Request::new(Action::Status),
            repo,
            repo,
            repo,
            stop,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        context.panel_main_attempt = owner.clone();
        context.participant = owner.clone();
        context.room_id = "room-team".into();
        let plan = Plan::open(context, &services.control, stop).unwrap();
        let call = |args: &[&str]| {
            plan.execute(
                &Request::parse(&args.iter().map(OsString::from).collect::<Vec<_>>()).unwrap(),
            )
        };
        call(&["init", "--goal", "bounded resource retirement"]).unwrap();
        call(&[
            "add",
            "--id",
            "task-a",
            "--title",
            "retire candidate",
            "--allowed",
            "src",
            "--verify",
            "true",
        ])
        .unwrap();
        let snapshot = plan.load().unwrap();
        let task = snapshot.plan.get("tasks").get("task-a");
        let (events, _) = oms_command_operations::load_projection(repo).unwrap();
        let generation = events
            .iter()
            .find(|event| {
                event.get("attempt_id").text() == Some(&owner)
                    && event.get("event_type").text() == Some("attempt.created")
            })
            .unwrap()
            .get("event_id")
            .text()
            .unwrap();
        let mut spec: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../oms-command-plan/tests/fixtures/team-spec.json"
        ))
        .unwrap();
        spec["root_owner"] = owner.clone().into();
        spec["root_owner_generation"] = generation.into();
        spec["plan_id"] = snapshot.plan.text("plan_id").into();
        spec["task_sha256"] = json::hash(task).unwrap().into();
        spec["objective_sha256"] = json::sha(b"bounded resource retirement").into();
        spec["criterion_sha256"] = oms_command_plan::team::criterion_digest(&snapshot.plan, task)
            .unwrap()
            .into();
        spec["base_sha"] = git(&["rev-parse", "HEAD"]).into();
        spec["policy_ref"] = "policy.json".into();
        let policy_hash = json::sha(std::fs::read(repo.join("policy.json")).unwrap());
        spec["policy_sha256"] = policy_hash.clone().into();
        for member in spec["members"].as_array_mut().unwrap() {
            member["root_owner"] = owner.clone().into();
            if member["role"] == "lead" {
                member["logical_parent"] = owner.clone().into();
            }
            if member["role"] == "worker" {
                member["access"] = "write".into();
            }
            member["route"]["policy_sha256"] = policy_hash.clone().into();
        }
        let register = |spec: &serde_json::Value| {
            std::fs::write(repo.join("team.json"), serde_json::to_vec(spec).unwrap()).unwrap();
            call(&["team-register", "--team-spec", "team.json"]).unwrap();
            let view = call(&["team-show", "--id", spec["id"].as_str().unwrap()]).unwrap();
            json::parse(&view.stdout)
                .unwrap()
                .get("history")
                .array()
                .unwrap()[0]
                .text("sha256")
                .to_owned()
        };
        let digest = register(&spec);
        let binding = resources::Binding {
            policy_sha256: policy.sha256().unwrap(),
            host_generation_sha256: policy.host_generation_sha256.clone(),
            root_owner: owner.clone(),
            root_generation: generation.into(),
            team: Some(resources::Team {
                id: "team-a".into(),
                spec_sha256: digest.clone(),
                member: "worker-a".into(),
                member_generation: 1,
            }),
            reviewer: false,
            persistent_session: false,
            cleanup_scopes: vec![
                oms_supervisor::process::Scope::ProcessGroup,
                oms_supervisor::process::Scope::WholeTree,
            ],
        };
        let deadline = || Instant::now() + Duration::from_secs(5);
        resources::reserve_team(repo, &binding, stop, deadline()).unwrap();
        assert!(matches!(
            physical::begin_bound_checked(
                repo,
                physical::Request {
                    identity: usage::Identity {
                        call_id: "call-resource".into(),
                        operation_id: "operation-resource".into(),
                        provider: "codex".into(),
                        task_id: Some("task-a".into()),
                        ..Default::default()
                    },
                    cumulative: None,
                    write: true,
                    campaign_manifest_sha256: None,
                },
                &binding,
                stop,
                deadline(),
                |_, _| Ok(())
            )
            .unwrap(),
            physical::Registration::New(_)
        ));
        let pending_usage = std::fs::read(repo.join(usage::RECEIPTS)).unwrap();
        let candidate = repo.join("src/candidate");
        std::fs::write(&candidate, b"unfinished candidate evidence\n").unwrap();
        let child = |words: &[&str], expected: i32, caller: &str| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--nocapture"])
                .env("OMS_RESOURCE_FIXTURE_REPO", repo)
                .env(
                    "OMS_RESOURCE_FIXTURE_ARGS",
                    serde_json::to_string(words).unwrap(),
                )
                .env("OMS_RESOURCE_FIXTURE_EXIT", expected.to_string())
                .env("OMS_PANEL_MAIN_ATTEMPT", caller)
                .env("OMS_ROOM_ID", "room-team")
                .env("OMS_ROOM_PARTICIPANT", caller)
                .env_remove("OMS_HARNESS_CHILD")
                .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
                .env_remove("OMS_STATE_REPO")
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let discard = [
            "resources",
            "discard-write",
            "--operation-id",
            "operation-resource",
            "--expected-revision",
            "1",
            "--reason",
            "candidate superseded",
        ];
        child(&discard, 75, &owner);
        let retire = [
            "resources",
            "retire-team",
            "--team-id",
            "team-a",
            "--team-spec-sha256",
            &digest,
            "--expected-revision",
            "1",
        ];
        child(&retire, 75, &owner);
        let tag = oms_supervisor::process::PhysicalCall::new("call-resource").unwrap();
        let done = Native
            .execute(
                Invocation {
                    argv: vec!["sh".into(), "-c".into(), "printf resource-fixture".into()],
                    cwd: repo.into(),
                    input: None,
                    environment: vec![],
                    remove_environment: vec![],
                    timeout: Duration::from_secs(5),
                    max_bytes: 1024,
                    merge: false,
                    physical_call: Some(tag.clone()),
                    launch_fence: None,
                },
                stop,
            )
            .unwrap();
        assert_eq!(done.code, 0);
        let cancelled = call(&[
            "team-cancel",
            "--id",
            "team-a",
            "--expected-team-sha256",
            &digest,
            "--reason",
            "withdraw managed work",
        ])
        .unwrap();
        let cancelled = json::parse(&cancelled.stdout).unwrap();
        let pending = cancelled.get("team-a").get("resource_retirement");
        assert_eq!(pending.text("status"), "pending");
        assert!(
            pending.get("execution_pending").truth() && pending.get("candidate_pending").truth()
        );
        assert_eq!(
            resources::snapshot(repo, stop)
                .unwrap()
                .unwrap()
                .occupancy
                .teams,
            1
        );
        child(&retire, 2, &owner);
        resources::settle_execution(
            repo,
            &binding,
            &tag,
            done.owned_cleanup.as_ref().unwrap(),
            1,
            stop,
            deadline(),
        )
        .unwrap();
        let report = team_withdrawn(&plan.context, "team-a", &digest, &services.control, stop)
            .unwrap()
            .unwrap();
        assert_eq!(report.text("status"), "pending");
        assert!(
            !report.get("execution_pending").truth() && report.get("candidate_pending").truth()
        );
        child(&retire, 2, &owner);
        let mut successor = spec.clone();
        successor["generation"] = 2.into();
        successor["previous_sha256"] = digest.clone().into();
        std::fs::write(
            repo.join("team.json"),
            serde_json::to_vec(&successor).unwrap(),
        )
        .unwrap();
        call(&[
            "team-register",
            "--team-spec",
            "team.json",
            "--expected-team-sha256",
            &digest,
        ])
        .unwrap();
        child(&discard, 2, "foreign-main");
        child(&discard, 0, &owner);
        child(&retire, 0, &owner);
        let retired = std::fs::read(repo.join(dispatch::REGISTRY)).unwrap();
        child(&discard, 0, &owner);
        assert_eq!(
            retired,
            std::fs::read(repo.join(dispatch::REGISTRY)).unwrap(),
            "exact retry is idempotent"
        );
        let mut changed = discard;
        changed[7] = "different disposition";
        child(&changed, 2, &owner);
        assert_eq!(
            std::fs::read(&candidate).unwrap(),
            b"unfinished candidate evidence\n"
        );
        assert_eq!(
            std::fs::read(repo.join(usage::RECEIPTS)).unwrap(),
            pending_usage,
            "resource release cannot erase unknown usage"
        );
        spec["id"] = "team-b".into();
        let second = register(&spec);
        let mut second_binding = binding;
        second_binding.team.as_mut().unwrap().id = "team-b".into();
        second_binding.team.as_mut().unwrap().spec_sha256 = second.clone();
        resources::reserve_team(repo, &second_binding, stop, deadline()).unwrap();
        let cancelled = call(&[
            "team-cancel",
            "--id",
            "team-b",
            "--expected-team-sha256",
            &second,
            "--reason",
            "withdraw before first launch",
        ])
        .unwrap();
        assert_eq!(
            json::parse(&cancelled.stdout)
                .unwrap()
                .get("team-b")
                .get("resource_retirement")
                .text("status"),
            "retired"
        );
        assert_eq!(
            resources::snapshot(repo, stop).unwrap().unwrap().occupancy,
            resources::Occupancy::default()
        );
    }
}
