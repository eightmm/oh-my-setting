#![forbid(unsafe_code)]
use oms_runtime::services::{RepositoryView, RuntimeRoots};
use oms_runtime::{Result, Services, Unavailable, run};
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oms-runtime-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&path)
                .status()
                .unwrap()
                .success()
        );
        Self(path)
    }
    fn run(&self, args: &[&str], interrupted: bool) -> oms_runtime::Response {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        run(
            &args,
            &self.0,
            &self.0,
            &Native,
            &AtomicBool::new(interrupted),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Native;
impl Services for Native {
    fn resolve_roots(&self, cwd: &Path, _: &OsStr) -> Result<RuntimeRoots> {
        Ok(RuntimeRoots {
            execution_repo: cwd.into(),
            state_repo: cwd.into(),
        })
    }
    fn host_name(&self) -> Result<String> {
        Ok("test-host".into())
    }
    fn repository_view(&self, _: &Path) -> Result<RepositoryView> {
        Ok(RepositoryView {
            head: String::new(),
            branch: String::new(),
            worktree_state: String::new(),
        })
    }
}
fn child_test(name: &str) -> bool {
    if std::env::var_os("OMS_RUNTIME_TEST_CHILD").is_some() {
        return false;
    }
    let f = Fixture::new();
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1") {
        assert_eq!(f.run(&["profile", "install", "core"], false).code, 2);
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name])
        .env_remove("OMS_HARNESS_CHILD")
        .env("OMS_RUNTIME_TEST_CHILD", "1")
        .env("OMS_LOCK_DIR", f.0.join("locks"))
        .status()
        .unwrap();
    assert!(status.success());
    true
}
#[test]
fn launch_dry_run_uses_native_command_and_forwards_options() {
    if child_test("launch_dry_run_uses_native_command_and_forwards_options") {
        return;
    }
    help_parse_interruption_and_unavailable_fail_closed();
    budget_contract();
    let f = Fixture::new();
    let args = [
        "experiment",
        "launch",
        "--question",
        "q",
        "--hypothesis",
        "h",
        "--prediction",
        "p",
        "--baseline",
        "b",
        "--metric",
        "m",
        "--success",
        "s",
        "--change",
        "c",
        "--dry-run",
        "--file",
        "data",
        "--metrics",
        "metrics",
        "--no-gate",
        "--reason",
        "reason",
        "--",
        "echo",
        "hello world",
    ];
    let response = f.run(&args, false);
    assert_eq!(
        response.code,
        0,
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let text = String::from_utf8(response.stdout).unwrap();
    assert!(text.contains("command: oms run-ledger --note"), "{text}");
    assert!(
        text.contains(
            "--file data --metrics metrics --no-gate --reason reason -- echo 'hello world'"
        ),
        "{text}"
    );
}
fn help_parse_interruption_and_unavailable_fail_closed() {
    let f = Fixture::new();
    assert_eq!(f.run(&["--help"], false).code, 0);
    assert_eq!(f.run(&["--version"], false).code, 0);
    assert_eq!(f.run(&["envelope", "--version"], false).code, 2);
    assert_eq!(f.run(&["bad-action"], false).code, 2);
    assert_eq!(f.run(&["profile", "list"], true).code, 130);
    let args = [OsString::from("profile"), OsString::from("list")];
    let response = run(&args, &f.0, &f.0, &Unavailable, &AtomicBool::new(false));
    assert_eq!(response.code, 2);
    assert!(String::from_utf8_lossy(&response.stderr).contains("unavailable"));
}

fn budget_contract() {
    let f = Fixture::new();
    let raw = br#"{"schema":2,"question":"q","hypothesis":"h","prediction":"p","baseline":{"x":1},"treatment":{"independent_change":"x"},"controlled_variables":{},"metrics":{"primary":{"name":"score","direction":"maximize"}},"seeds":[0],"success":{"min_improvement":0},"budget":{"runs":1}}"#;
    let contract =
        oms_runtime::experiment::validate(&oms_runtime::value::parse(raw).unwrap()).unwrap();
    std::fs::write(f.0.join("contract.json"), raw).unwrap();
    std::fs::set_permissions(
        f.0.join("contract.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let dir = f.0.join(".oms/experiments");
    std::fs::create_dir_all(&dir).unwrap();
    for path in [f.0.join(".oms"), dir.clone()] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let digest = contract.text("contract_digest");
    let rows = format!(
        "{{\"run_id\":\"one\",\"contract_digest\":\"{digest}\"}}\n{{\"run_id\":\"two\",\"contract_digest\":\"{digest}\"}}\n"
    );
    std::fs::write(dir.join("runs.jsonl"), rows).unwrap();
    std::fs::set_permissions(
        dir.join("runs.jsonl"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let args = [
        "experiment",
        "launch",
        "--contract",
        "contract.json",
        "--dry-run",
        "--",
        "true",
    ];
    let refused = f.run(&args, false);
    assert_eq!(
        refused.code,
        2,
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("run budget spent: 2 of 1 run(s) recorded for this contract"),
        "{message}"
    );
    assert!(!message.contains("0 reserved"));
    let allowed = f.run(
        &[
            "experiment",
            "launch",
            "--contract",
            "contract.json",
            "--dry-run",
            "--reason",
            "retry",
            "--",
            "true",
        ],
        false,
    );
    assert_eq!(
        allowed.code,
        0,
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&allowed.stderr).contains("past the contract run budget (retry)")
    );
    let held =
        format!("{{\"kind\":\"reserved\",\"run_id\":\"held\",\"contract_digest\":\"{digest}\"}}\n");
    let reservation_path = dir.join("budget-reservations.jsonl");
    std::fs::write(&reservation_path, held).unwrap();
    std::fs::set_permissions(&reservation_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let refused = f.run(&args, false);
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("3 of 1 run(s) recorded or reserved"),
        "{message}"
    );
    assert!(
        message.contains("1 reserved by a run in progress"),
        "{message}"
    );
    std::fs::write(&reservation_path, b"").unwrap();
    std::fs::write(dir.join("runs.jsonl"), b"").unwrap();
    let failure = f.run(
        &[
            "experiment",
            "launch",
            "--contract",
            "contract.json",
            "--",
            "true",
        ],
        false,
    );
    assert_eq!(
        failure.code,
        2,
        "{}",
        String::from_utf8_lossy(&failure.stderr)
    );
    assert!(String::from_utf8_lossy(&failure.stderr).contains("run ledger"));
    let reservations = std::fs::read_to_string(dir.join("budget-reservations.jsonl")).unwrap();
    assert!(reservations.contains("\"reserved\""));
    assert!(reservations.contains("\"released\""));
}

fn profile_planning_and_install_fail_closed() {
    let f = Fixture::new();
    let plan = f.run(&["profile", "install-plan", "core"], false);
    assert_eq!(plan.code, 0, "{}", String::from_utf8_lossy(&plan.stderr));
    assert!(String::from_utf8_lossy(&plan.stdout).contains("scripts/install-profile.sh"));
    assert_eq!(
        f.run(&["profile", "install-plan", "missing"], false).code,
        2
    );
    assert_eq!(
        f.run(
            &[
                "profile",
                "install-plan",
                "--primary-provider",
                "missing",
                "core"
            ],
            false
        )
        .code,
        2
    );
    let check = f.run(&["profile", "check", "core"], false);
    assert!([0, 3].contains(&check.code));
    let applied = f.run(&["profile", "apply", "--allow-missing", "core"], false);
    assert!(
        [0, 3].contains(&applied.code),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let current = f.run(&["profile", "current"], false);
    assert!([0, 3].contains(&current.code));
    assert!(String::from_utf8_lossy(&current.stdout).contains("configured"));
    let install = f.run(&["profile", "install", "core"], false);
    assert_eq!(install.code, 2);
    assert!(String::from_utf8_lossy(&install.stderr).contains("unavailable"));
}

#[test]
fn release_promotion_rejects_invalid_inputs() {
    if child_test("release_promotion_rejects_invalid_inputs") {
        return;
    }
    profile_planning_and_install_fail_closed();
    let f = Fixture::new();
    let commit = "a".repeat(40);
    let digest = "b".repeat(64);
    for (sha, version, hash) in [
        ("", "1.2.3", digest.as_str()),
        (commit.as_str(), "bad", digest.as_str()),
        (commit.as_str(), "1.2.3", "bad"),
    ] {
        let response = f.run(
            &[
                "release",
                "promote",
                "--commit",
                sha,
                "--version",
                version,
                "--expected-manifest-digest",
                hash,
            ],
            false,
        );
        assert_eq!(
            response.code,
            2,
            "{sha:?} {version:?} {hash:?} stdout={} stderr={}",
            String::from_utf8_lossy(&response.stdout),
            String::from_utf8_lossy(&response.stderr)
        );
    }
    let interrupted = AtomicBool::new(false);
    let runtime =
        oms_runtime::storage::Runtime::open(&f.0, &f.0, &f.0, &Native, &interrupted).unwrap();
    let manifest = oms_runtime::release::manifest(&runtime).unwrap();
    let reviewed = oms_runtime::value::hash(&manifest).unwrap();
    let matching = f.run(
        &[
            "release",
            "promote",
            "--commit",
            &commit,
            "--version=1.2.3",
            "--expected-manifest-digest",
            &reviewed,
        ],
        false,
    );
    assert_eq!(matching.code, 2);
    assert!(String::from_utf8_lossy(&matching.stderr).contains("release promotion"));
    let response = f.run(
        &[
            "release",
            "promote",
            "--commit",
            &commit,
            "--version",
            "1.2.3",
            "--expected-manifest-digest",
            &digest,
        ],
        false,
    );
    assert_eq!(response.code, 75);
}
