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
    ]);
    if let Some(snapshot) = snapshot {
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
) -> Result<Value> {
    // Root resolution occurs before this producer. This deadline bounds the
    // actual boot proof and mutation writer, not the whole public command.
    let deadline = Instant::now() + Duration::from_secs(10);
    before(stop, deadline)?;
    let mutation = matches!(
        action,
        ResourceAction::Activate { .. } | ResourceAction::Hold { .. }
    );
    if mutation {
        parent()?;
    }
    let host = super::observed_host_generation_before(deadline, stop)?;
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
    }
    before(stop, deadline)?;
    if super::observed_host_generation_before(deadline, stop)? != host {
        return Err(error(
            75,
            "resource host generation changed; reconcile current custody",
        ));
    }
    let report = status(state_repo, &host, stop)?;
    before(stop, deadline)?;
    Ok(report)
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
                    flag.as_ref()
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

        let shared = root.0.join("shared-state");
        std::fs::create_dir(&shared).unwrap();
        let activated = action(
            &root.0,
            &shared,
            &ResourceAction::Activate {
                policy: selected.clone(),
            },
            flag.as_ref(),
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
                flag.as_ref()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(occupied.join(dispatch::REGISTRY)).unwrap(),
            history
        );
    }
}
