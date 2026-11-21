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
    fn task_status(&self, _: &Path, _: &Path, _: &[u8]) -> Result<Vec<u8>> {
        Ok(
            br#"{"schema":1,"present":true,"status":"unknown","verification":"unverified","stale":false}"#
                .to_vec(),
        )
    }
    fn resource_action(
        &self,
        repo: &Path,
        state_repo: &Path,
        action: &oms_runtime::services::ResourceAction,
    ) -> Result<oms_runtime::value::Value> {
        use oms_runtime::services::ResourceAction;
        use oms_runtime::value::*;
        assert_eq!(repo, state_repo);
        let (name, policy) = match action {
            ResourceAction::Host => ("host", Value::Null),
            ResourceAction::Status => ("status", Value::Null),
            ResourceAction::Activate { policy } => ("activate", os(policy.as_os_str())),
            ResourceAction::Hold {
                expected_policy_sha256,
            } => {
                assert_eq!(expected_policy_sha256, &"a".repeat(64));
                ("hold", Value::Null)
            }
        };
        Ok(object([("action", string(name)), ("policy", policy)]))
    }
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
    fn backend_check(
        &self,
        profile: &str,
        image: &OsStr,
        adapter: &OsStr,
    ) -> Result<oms_runtime::value::Value> {
        if profile != "apptainer" {
            return Unavailable.backend_check(profile, image, adapter);
        }
        assert!(Path::new(image).is_absolute() && adapter.is_empty());
        use oms_runtime::value::*;
        Ok(object([
            ("ready", b(true)),
            ("missing", a([])),
            ("details", empty()),
        ]))
    }
    fn supervise(
        &self,
        repo: &Path,
        request: &oms_runtime::services::BackendRequest,
    ) -> Result<oms_runtime::services::BackendResult> {
        if request.profile != "apptainer" {
            return Unavailable.supervise(repo, request);
        }
        use oms_runtime::value::*;
        // This fixture verifies the public parser/producer boundary, not a launch.
        Ok(oms_runtime::services::BackendResult {
            exit: 7,
            receipt: object([
                ("profile", string(&request.profile)),
                ("image", string(request.image.to_string_lossy())),
                (
                    "argv",
                    a(request.argv.iter().map(|s| string(s.to_string_lossy()))),
                ),
            ]),
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
    let status = std::process::Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "umask 077; exec \"$@\"",
            "--",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name])
        .env_remove("OMS_HARNESS_CHILD")
        .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
        .env("OMS_RUNTIME_TEST_CHILD", "1")
        .env("OMS_LOCK_DIR", f.0.join("locks"))
        .status()
        .unwrap();
    assert!(status.success());
    true
}

#[test]
fn context_keeps_cached_memory_summary_explicit() {
    if child_test("context_keeps_cached_memory_summary_explicit") {
        return;
    }
    let f = Fixture::new();
    for directory in ["config", ".oms", ".oms/memory"] {
        let path = f.0.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (path, body) in [
        (
            "config/context-phases.json",
            include_bytes!("../../../config/context-phases.json").as_slice(),
        ),
        (
            ".oms/memory/pins.md",
            b"Explicitly authored pin\n".as_slice(),
        ),
        (
            ".oms/memory/summary.md",
            b"Cached unreviewed task outcome\n".as_slice(),
        ),
    ] {
        let path = f.0.join(path);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    for explicit in [false, true] {
        let mut args = vec!["context", "--phase", "orientation"];
        if explicit {
            args.extend(["--file", ".oms/memory/summary.md"]);
        }
        let response = f.run(&args, false);
        assert_eq!(
            response.code,
            0,
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let manifest = oms_runtime::value::parse(&response.stdout).unwrap();
        let bundle = std::fs::read_to_string(f.0.join(manifest.text("bundle_path"))).unwrap();
        let saved = std::fs::read(f.0.join(manifest.text("manifest_path"))).unwrap();
        let inspected = oms_context::inspect_manifest(&saved, 100).unwrap();
        let a = &inspected["request_attribution"];
        assert_eq!(a["request_bytes"], bundle.len());
        assert_eq!(
            a["request_sha256"],
            oms_runtime::value::sha(bundle.as_bytes())
        );
        assert_eq!(
            a["phase_policy_sha256"],
            manifest.text("phase_rules_sha256")
        );
        assert_eq!(a["measured_bytes"], bundle.len());
        assert_eq!(a["unattributed_bytes"], 0);
        for span in a["spans"]["items"].as_array().unwrap() {
            let start = span["start"].as_u64().unwrap() as usize;
            let end = span["end"].as_u64().unwrap() as usize;
            assert_eq!(
                span["sha256"],
                oms_runtime::value::sha(&bundle.as_bytes()[start..end])
            );
            assert_eq!(
                span["prefix_sha256"],
                oms_runtime::value::sha(&bundle.as_bytes()[..end])
            );
        }
        assert!(bundle.contains("Explicitly authored pin"), "{bundle}");
        assert_eq!(
            bundle.contains("Cached unreviewed task outcome"),
            explicit,
            "{bundle}"
        );
        assert_eq!(
            std::fs::read(f.0.join(".oms/memory/summary.md")).unwrap(),
            b"Cached unreviewed task outcome\n"
        );
    }
    std::fs::create_dir(f.0.join(".oms/task")).unwrap();
    let source = (0..4000)
        .map(|i| format!("문맥 {i} 🙂\n"))
        .collect::<String>();
    let mut observed = Vec::new();
    for (path, policy) in [
        ("PROJECT.md", "head"),
        (".oms/task/current.md", "tail"),
        ("AGENTS.md", "middle"),
    ] {
        std::fs::write(f.0.join(path), &source).unwrap();
        let response = f.run(&["context", "--max-bytes", "4096"], false);
        assert_eq!(
            response.code,
            if path == ".oms/task/current.md" { 0 } else { 3 },
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let manifest = oms_runtime::value::parse(&response.stdout).unwrap();
        let bundle = std::fs::read(f.0.join(manifest.text("bundle_path"))).unwrap();
        assert!(
            std::str::from_utf8(&bundle).is_ok(),
            "{policy} split a UTF-8 character"
        );
        let saved = std::fs::read(f.0.join(manifest.text("manifest_path"))).unwrap();
        let inspected = oms_context::inspect_manifest(&saved, 100).unwrap();
        let a = &inspected["request_attribution"];
        assert_eq!(a["request_bytes"], bundle.len());
        assert_eq!(a["measured_bytes"], bundle.len());
        assert!(
            a["layers"]["framing"]["bytes"].as_u64().unwrap()
                > inspected["budget"]["framing_bytes"].as_u64().unwrap()
        );
        let mut ranges = Vec::new();
        for span in a["spans"]["items"].as_array().unwrap() {
            let start = span["start"].as_u64().unwrap() as usize;
            let end = span["end"].as_u64().unwrap() as usize;
            assert_eq!(span["sha256"], oms_runtime::value::sha(&bundle[start..end]));
            assert_eq!(
                span["prefix_sha256"],
                oms_runtime::value::sha(&bundle[..end])
            );
            if span["source_sha256"] == oms_runtime::value::sha(source.as_bytes()) {
                let r = span["source_range"].as_array().unwrap();
                let lo = r[0].as_u64().unwrap() as usize;
                let hi = r[1].as_u64().unwrap() as usize;
                assert_eq!(&bundle[start..end], &source.as_bytes()[lo..hi]);
                ranges.push((lo, hi));
            }
        }
        assert_eq!(ranges.len(), if policy == "middle" { 2 } else { 1 });
        observed.push(ranges);
        let inspected_again = f.run(&["context-inspect", manifest.text("manifest_path")], false);
        assert_eq!(inspected_again.code, 0);
        assert_eq!(
            inspected["debt"]["reported"],
            manifest.get("context_debt").int().unwrap() as u64
        );
        std::fs::remove_file(f.0.join(path)).unwrap();
    }
    assert_ne!(observed[0], observed[1]);
    assert_ne!(observed[0], observed[2]);
    let attributed = oms_context::attribution::describe(
        "작업 🙂".as_bytes(),
        "fixture-v1",
        &[],
        |raw| oms_runtime::value::sha(raw),
        None,
        None,
    )
    .unwrap();
    let request = serde_json::json!({"schema":1,"kind":"oms-context-request","operation_id":"op-fixture","call_id":"call-fixture",
        "provider":"fixture","dispatch":"prepared","provider_delivery":null,"attribution":attributed});
    std::fs::write(
        f.0.join("call-context.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let response = f.run(&["context-inspect", "call-context.json"], false);
    assert_eq!(
        response.code,
        0,
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let projection: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(projection["kind"], "oms-context-request-inspection");
    assert!(projection["provider_delivery"].is_null());
    let response = f.run(
        &["context-inspect", "call-context.json", "--format", "text"],
        false,
    );
    assert_eq!(response.code, 0);
    assert!(
        String::from_utf8_lossy(&response.stdout)
            .contains("Provider delivery and model use: unknown")
    );
    assert_eq!(
        f.run(
            &["context-inspect", "call-context.json", "--verify-current"],
            false
        )
        .code,
        2
    );
    assert_eq!(
        f.run(
            &[
                "context-inspect",
                "call-context.json",
                "--compare",
                "call-context.json"
            ],
            false
        )
        .code,
        2
    );
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
    assert!(
        Unavailable
            .resource_action(&f.0, &f.0, &oms_runtime::services::ResourceAction::Status)
            .is_err()
    );
    for action in ["host", "status", "activate", "hold"] {
        let help = f.run(&["resources", action, "--help"], false);
        assert_eq!(help.code, 0);
        assert!(
            String::from_utf8_lossy(&help.stdout)
                .contains(&format!("oms runtime resources {action}"))
        );
    }
    for invalid in [
        vec!["resources"],
        vec!["resources", "activate"],
        vec!["resources", "activate", "--policy", ""],
        vec!["resources", "hold", "--expected-policy-sha256", "bad"],
        vec!["resources", "retire", "--done", "true"],
    ] {
        assert_eq!(f.run(&invalid, false).code, 2);
    }
    for action in ["host", "status"] {
        let report = f.run(&["resources", action], false);
        assert_eq!(report.code, 0);
        assert_eq!(
            oms_runtime::value::parse(&report.stdout)
                .unwrap()
                .text("action"),
            action
        );
        let parsed = oms_runtime::parse(&["resources".into(), action.into()]).unwrap();
        assert!(parsed.child_read_only());
    }
    let activated = f.run(
        &[
            "resources",
            "activate",
            "--policy",
            f.0.join("policy.json").to_str().unwrap(),
        ],
        false,
    );
    assert_eq!(activated.code, 0);
    assert_eq!(
        oms_runtime::value::parse(&activated.stdout)
            .unwrap()
            .text("policy"),
        "policy.json"
    );
    let held = f.run(
        &[
            "resources",
            "hold",
            "--expected-policy-sha256",
            &"a".repeat(64),
        ],
        false,
    );
    assert_eq!(held.code, 0);
    assert_eq!(
        oms_runtime::value::parse(&held.stdout)
            .unwrap()
            .text("action"),
        "hold"
    );
    let described = f.run(&["backend", "describe", "apptainer"], false);
    assert_eq!(described.code, 0);
    assert!(String::from_utf8_lossy(&described.stdout).contains("cgroup enforcement"));
    for extra in [
        vec![],
        vec!["--image", "docker://example/image"],
        vec!["--image", "image.sif", "--inherit-env", "APPTAINER_BIND"],
        vec!["--image", "image.sif", "--tmpfs-mb", "64"],
    ] {
        let mut words = vec!["backend", "run", "apptainer"];
        words.extend(extra);
        words.extend(["--", "true"]);
        let out = f.run(&words, false);
        assert_eq!(out.code, 2);
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("integration pending"),
            "request must refuse before the producer: {:?}",
            out.stderr
        );
    }
    let response = f.run(
        &[
            "backend",
            "run",
            "apptainer",
            "--image",
            "local.sif",
            "--",
            "tool",
            "two words",
            "$(literal)",
        ],
        false,
    );
    assert_eq!(
        response.code,
        7,
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let receipt = oms_runtime::value::parse(&response.stdout).unwrap();
    assert_eq!(receipt.text("profile"), "apptainer");
    assert_eq!(
        receipt.text("image"),
        f.0.join("local.sif").to_str().unwrap()
    );
    assert_eq!(
        receipt.get("argv").strings().unwrap(),
        vec!["tool", "two words", "$(literal)"]
    );
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
fn release_defaults_follow_remote_head_without_inventing_stable() {
    if child_test("release_defaults_follow_remote_head_without_inventing_stable") {
        return;
    }
    use oms_runtime::services::{ExternalOutput, ReleaseRequest};
    use oms_runtime::value::{Value, from_serde, hash};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Releases {
        revisions: RefCell<Vec<String>>,
        fetches: Cell<u64>,
        applications: RefCell<Vec<ReleaseRequest>>,
    }
    impl Services for Releases {
        fn repository_view(&self, repo: &Path) -> Result<RepositoryView> {
            Native.repository_view(repo)
        }
        fn resolve_revision(&self, root: &Path, revision: &str) -> Result<Option<String>> {
            assert!(!revision.is_empty());
            self.revisions.borrow_mut().push(revision.into());
            let output = std::process::Command::new("git")
                .args([
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{revision}^{{commit}}"),
                ])
                .current_dir(root)
                .output()
                .unwrap();
            Ok(output
                .status
                .success()
                .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned()))
        }
        fn fetch_release(&self, _: &Path) -> Result<()> {
            self.fetches.set(self.fetches.get() + 1);
            Ok(())
        }
        fn apply_release(&self, _: &Path, request: &ReleaseRequest) -> Result<ExternalOutput> {
            self.applications.borrow_mut().push(request.clone());
            Ok(ExternalOutput {
                exit: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    let f = Fixture::new();
    for args in [
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ][..],
        &["update-ref", "refs/remotes/origin/development", "HEAD"][..],
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/development",
        ][..],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&f.0)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(
        f.0.join("PROJECT.md"),
        "# Release fixture\n\n## Acceptance\n- Update channels report their configured state.\n",
    )
    .unwrap();
    let mut profiles: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/capability-profiles.json")).unwrap();
    profiles["profiles"]["core"] = serde_json::json!({
        "required": [], "optional": [], "any_of": [], "inherits": []
    });
    std::fs::create_dir(f.0.join("config")).unwrap();
    std::fs::write(
        f.0.join("config/capability-profiles.json"),
        serde_json::to_vec(&profiles).unwrap(),
    )
    .unwrap();
    let services = Releases::default();
    let interrupted = AtomicBool::new(false);
    let runtime =
        oms_runtime::storage::Runtime::open(&f.0, &f.0, &f.0, &services, &interrupted).unwrap();
    let default = oms_runtime::release::manifest(&runtime).unwrap();
    let reviewed = hash(&default).unwrap();
    let status = oms_runtime::release::status(&runtime).unwrap();
    let stable = status.get("stable");
    assert!(!stable.get("ready").truth());
    for field in [
        "configured_ref",
        "selected_ref",
        "resolved_commit",
        "version",
        "apply_command",
    ] {
        assert!(matches!(stable.get(field), Value::Null), "{field}");
    }
    let edge = status.get("edge");
    assert!(edge.get("ready").truth());
    assert_eq!(edge.text("configured_ref"), "origin/HEAD");
    assert_eq!(edge.text("selected_ref"), "origin/HEAD");
    assert_eq!(edge.text("apply_command"), "oms runtime release apply edge");
    let command: Vec<OsString> = edge
        .text("apply_command")
        .split_whitespace()
        .skip(2)
        .map(OsString::from)
        .collect();
    let request = oms_runtime::parse(&command).unwrap();
    assert_eq!(request.mode, oms_runtime::parser::Mode::Release);
    assert_eq!(request.action, "apply");
    assert_eq!(request.positional(0).unwrap(), "edge");
    let commit = edge.text("resolved_commit").to_owned();
    assert!([40, 64].contains(&commit.len()));

    let refused = oms_runtime::release::apply(&runtime, "stable", true, true).unwrap_err();
    assert_eq!(refused.code, 3);
    assert_eq!(services.fetches.get(), 0);
    assert!(services.applications.borrow().is_empty());
    assert!(!f.0.join("config/update-channels.json").exists());
    assert!(
        services
            .revisions
            .borrow()
            .iter()
            .all(|r| r == "HEAD" || r == "origin/HEAD")
    );
    let doctor_request = oms_runtime::parse(&["doctor".into(), "--strict".into()]).unwrap();
    let doctor = || {
        let response = oms_runtime::router::dispatch(&runtime, &doctor_request).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        (response.code, report)
    };
    let (code, report) = doctor();
    assert_eq!(code, 0);
    assert_eq!(report["ok"], true);
    assert_eq!(report["issues"], serde_json::json!([]));
    assert_eq!(report["release"]["stable"]["ready"], false);
    assert!(report["release"]["stable"]["configured_ref"].is_null());

    let configured = serde_json::json!({
        "schema": 1,
        "channels": {
            "stable": {
                "ref": commit,
                "version": "1.2.3",
                "auto_apply": false,
                "policy": "pinned reviewed commit",
                "promoted_at": "2026-08-18T03:58:43Z"
            },
            "edge": {"ref": "development", "version": "edge", "auto_apply": false}
        }
    });
    let manifest_path = f.0.join("config/update-channels.json");
    let configured_bytes = serde_json::to_vec(&configured).unwrap();
    std::fs::write(&manifest_path, &configured_bytes).unwrap();
    let actual = oms_runtime::release::manifest(&runtime).unwrap();
    assert_eq!(
        hash(&actual).unwrap(),
        hash(&from_serde(&configured).unwrap()).unwrap()
    );
    let stable = oms_runtime::release::resolve(&runtime, "stable", false).unwrap();
    assert!(stable.get("ready").truth());
    assert_eq!(stable.text("configured_ref"), commit);
    assert_eq!(stable.text("resolved_commit"), commit);
    assert_eq!(stable.text("version"), "1.2.3");
    assert_eq!(
        stable.text("apply_command"),
        format!("oms update --ref {commit}")
    );
    let edge = oms_runtime::release::resolve(&runtime, "edge", false).unwrap();
    assert_eq!(edge.text("configured_ref"), "development");
    assert_eq!(edge.text("selected_ref"), "origin/development");
    let stale = oms_runtime::release::promote(&runtime, &commit, "2.0.0", &reviewed).unwrap_err();
    assert_eq!(stale.code, 75);
    oms_runtime::release::apply(&runtime, "stable", false, true).unwrap();
    let applications = services.applications.borrow();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].channel, "stable");
    assert_eq!(applications[0].target, commit);
    assert_eq!(applications[0].selected_ref, commit);
    assert!(applications[0].no_tools);
    drop(applications);
    oms_runtime::release::apply(&runtime, "edge", false, false).unwrap();
    let applications = services.applications.borrow();
    assert_eq!(applications.len(), 2);
    assert_eq!(applications[1].channel, "edge");
    assert_eq!(applications[1].target, commit);
    assert_eq!(applications[1].selected_ref, "origin/development");
    assert!(!applications[1].no_tools);
    drop(applications);
    assert_eq!(std::fs::read(&manifest_path).unwrap(), configured_bytes);
    assert_eq!(doctor().0, 0);

    let mut unavailable = configured.clone();
    unavailable["channels"]["stable"]["ref"] = serde_json::json!("f".repeat(commit.len()));
    std::fs::write(&manifest_path, serde_json::to_vec(&unavailable).unwrap()).unwrap();
    let (code, report) = doctor();
    assert_eq!(code, 3);
    assert_eq!(report["ok"], false);
    assert_eq!(
        report["issues"],
        serde_json::json!(["configured stable channel ref is unavailable locally"])
    );

    for (channel, reference) in [
        ("stable", serde_json::Value::Null),
        ("stable", serde_json::json!("")),
        ("stable", serde_json::json!(17)),
        ("edge", serde_json::Value::Null),
    ] {
        let mut invalid = configured.clone();
        invalid["channels"][channel]["ref"] = reference;
        let bytes = serde_json::to_vec(&invalid).unwrap();
        std::fs::write(&manifest_path, &bytes).unwrap();
        assert_eq!(
            oms_runtime::release::manifest(&runtime).unwrap_err().code,
            2
        );
        assert_eq!(
            oms_runtime::release::apply(&runtime, "stable", true, true)
                .unwrap_err()
                .code,
            2
        );
        assert_eq!(services.fetches.get(), 0);
        assert_eq!(services.applications.borrow().len(), 2);
        assert_eq!(std::fs::read(&manifest_path).unwrap(), bytes);
    }
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

#[test]
fn snapshot_detects_checkout_and_lockfile_drift() {
    if child_test("snapshot_detects_checkout_and_lockfile_drift") {
        return;
    }
    let f = Fixture::new();
    std::fs::write(f.0.join("Cargo.lock"), "original\n").unwrap();
    for args in [
        &["add", "Cargo.lock"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "initial",
        ][..],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&f.0)
                .status()
                .unwrap()
                .success()
        );
    }
    let created = f.run(&["exec-snapshot", "create"], false);
    assert_eq!(
        created.code,
        0,
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let sha = snapshot["snapshot_sha256"].as_str().unwrap();
    assert_eq!(
        f.run(&["exec-snapshot", "verify", "--snapshot", sha], false)
            .code,
        0
    );
    std::fs::write(f.0.join("new-file"), "untracked").unwrap();
    assert_eq!(
        f.run(&["exec-snapshot", "verify", "--snapshot", sha], false)
            .code,
        2
    );
    assert_eq!(f.run(&["exec-snapshot", "create"], false).code, 2);
    std::fs::remove_file(f.0.join("new-file")).unwrap();
    std::fs::write(f.0.join("Cargo.lock"), "changed\n").unwrap();
    assert_eq!(
        f.run(&["exec-snapshot", "verify", "--snapshot", sha], false)
            .code,
        2
    );
}

#[test]
fn target_approval_is_bound_to_identity() {
    if child_test("target_approval_is_bound_to_identity") {
        return;
    }
    let f = Fixture::new();
    let dir = f.0.join(".oms/runtime");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("targets.json");
    std::fs::write(
        &path,
        r#"{"schema":1,"targets":{"node":{"ssh_alias":"node","workdir":"~/repo"}}}"#,
    )
    .unwrap();
    let check = || f.run(&["backend", "check", "remote", "--target", "node"], false);
    let result: serde_json::Value = serde_json::from_slice(&check().stdout).unwrap();
    assert_eq!(result["execution_approved"], false);
    let result = f.run(&["backend", "target", "approve", "node"], false);
    assert_eq!(
        result.code,
        0,
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&check().stdout).unwrap();
    assert_eq!(result["execution_approved"], true);
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["targets"]["node"]["ssh_alias"] = "other".into();
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&check().stdout).unwrap();
    assert_eq!(result["execution_approved"], false);
    assert_eq!(
        f.run(&["backend", "target", "revoke", "node"], false).code,
        0
    );
}

#[test]
fn remote_experiment_reconciles_exact_results_without_relaunch() {
    use std::process::Command;
    if std::env::var_os("OMS_TEST_REMOTE_EXPERIMENT").is_none() {
        let f = Fixture::new();
        let fake = f.0.join("ssh-fixture");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
printf 'call\n' >> "$OMS_TEST_REMOTE_EVENTS"
for last do :; done
if [ -f "$OMS_TEST_REMOTE_DROP" ]; then
  rm -f "$OMS_TEST_REMOTE_DROP"
  sh -c "$last" >/dev/null 2>&1
  exit 255
fi
exec sh -c "$last"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "remote_experiment_reconciles_exact_results_without_relaunch",
                "--nocapture",
            ])
            .env("OMS_TEST_REMOTE_EXPERIMENT", &f.0)
            .env("OMS_SSH_BIN", &fake)
            .env("OMS_TEST_REMOTE_EVENTS", f.0.join("calls"))
            .env("OMS_TEST_REMOTE_DROP", f.0.join("drop"))
            .env("OMS_LOCK_DIR", f.0.join("locks"))
            .env_remove("OMS_HARNESS_CHILD")
            .env_remove("OMS_HARNESS_DELEGATE_DEPTH")
            .env_remove("OMS_REMOTE_STATE_DIR")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let root = PathBuf::from(std::env::var_os("OMS_TEST_REMOTE_EXPERIMENT").unwrap());
    let f = Fixture::new();
    std::fs::write(f.0.join(".gitignore"), ".oms/\n").unwrap();
    for args in [
        &["add", ".gitignore"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&f.0)
                .status()
                .unwrap()
                .success()
        );
    }
    let remote = root.join("remote");
    assert!(
        Command::new("git")
            .arg("clone")
            .arg("-q")
            .arg(&f.0)
            .arg(&remote)
            .status()
            .unwrap()
            .success()
    );
    let snapshot = f.run(&["exec-snapshot", "create"], false);
    assert_eq!(
        snapshot.code,
        0,
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    let digest = snapshot["snapshot_sha256"].as_str().unwrap();
    let registry = serde_json::json!({"schema": 1, "targets": {"node": {
        "ssh_alias": "node", "workdir": remote.to_str().unwrap(), "scheduler": "none"
    }}});
    std::fs::write(
        f.0.join(".oms/runtime/targets.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let contract = r#"{"schema":2,"question":"q","hypothesis":"h","prediction":"p","baseline":{"channel":"local"},"treatment":{"independent_change":"channel"},"controlled_variables":{"data":"fixture"},"metrics":{"primary":{"name":"score","direction":"maximize"}},"seeds":[0,1],"success":{"min_improvement":0},"budget":{"runs":4}}"#;
    std::fs::write(f.0.join(".oms/contract.json"), contract).unwrap();
    let registered = f.run(
        &[
            "experiment",
            "register",
            ".oms/contract.json",
            "--id",
            "trial",
        ],
        false,
    );
    assert_eq!(
        registered.code,
        0,
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );
    let run = |run_id: &str, command: &str| {
        f.run(
            &[
                "experiment",
                "run",
                "--id",
                "trial",
                "--arm",
                "baseline",
                "--seed",
                "0",
                "--profile",
                "remote",
                "--target",
                "node",
                "--snapshot",
                digest,
                "--run-id",
                run_id,
                "--",
                "sh",
                "-c",
                command,
            ],
            false,
        )
    };
    let command = r#"printf 'ran\n' >> .git/executions; printf '{"score":1}' > "$OMS_EXPERIMENT_METRICS"; printf 'training output\n'"#;
    assert_eq!(run("experiment-run-first", command).code, 2);
    assert!(!root.join("calls").exists());
    assert_eq!(
        f.run(&["backend", "target", "approve", "node"], false).code,
        0
    );
    std::fs::write(root.join("drop"), "once").unwrap();
    let first = run("experiment-run-first", command);
    assert_eq!(first.code, 75, "{}", String::from_utf8_lossy(&first.stderr));
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["state"], "unknown");
    let calls = std::fs::read(root.join("calls")).unwrap();
    let ledger = f.0.join(".oms/experiments/runs.jsonl");
    let pending = std::fs::read(&ledger).unwrap();
    assert_eq!(run("experiment-run-first", command).code, 75);
    assert_eq!(std::fs::read(&ledger).unwrap(), pending);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    assert_eq!(run("experiment-run-first", "true").code, 2);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    let reconcile = |name| f.run(&["experiment", "reconcile", "--run-id", name], false);
    let receipt = f.0.join(first["execution_receipt"].as_str().unwrap());
    let saved = root.join("saved-operation-receipt");
    std::fs::rename(&receipt, &saved).unwrap();
    for result in [
        run("experiment-run-first", command),
        reconcile("experiment-run-first"),
        reconcile("experiment-run-first"),
    ] {
        assert_eq!(result.code, 75);
        let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(result["state"], "unknown");
        assert_eq!(result["reconcile_required"], true);
        assert_eq!(result["reconcile_error"], "missing_operation_receipt");
    }
    assert_eq!(std::fs::read(&ledger).unwrap(), pending);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    assert!(!receipt.exists());
    std::fs::rename(saved, receipt).unwrap();
    assert_eq!(
        f.run(&["backend", "target", "revoke", "node"], false).code,
        0
    );
    assert_eq!(reconcile("experiment-run-first").code, 2);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    assert_eq!(
        f.run(&["backend", "target", "approve", "node"], false).code,
        0
    );
    // Recovery observes the old operation even if the local checkout has since
    // changed; it cannot launch the payload against the changed source tree.
    std::fs::write(f.0.join(".gitignore"), ".oms/\n# changed after launch\n").unwrap();
    let recovered = reconcile("experiment-run-first");
    assert_eq!(
        recovered.code,
        0,
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let row: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(row["snapshot"], digest);
    assert_eq!(row["metrics"]["score"], 1.0);
    assert_eq!(row["metrics_valid"], true);
    assert_eq!(row["execution_operation_id"], "experiment-run-first");
    assert_eq!(row["result_bundle_sha256"].as_str().unwrap().len(), 64);
    let calls = std::fs::read(root.join("calls")).unwrap();
    let terminal = std::fs::read(&ledger).unwrap();
    assert_eq!(reconcile("experiment-run-first").code, 0);
    assert_eq!(run("experiment-run-first", command).code, 0);
    assert_eq!(run("experiment-run-first", "true").code, 2);
    assert_eq!(std::fs::read(&ledger).unwrap(), terminal);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    std::fs::write(f.0.join(".gitignore"), ".oms/\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(remote.join(".git/executions")).unwrap(),
        "ran\n"
    );

    // A real remote completion with a result from another run must never enter
    // the scientific comparison, even when its numbers look valid.
    std::fs::write(root.join("drop"), "once").unwrap();
    assert_eq!(run("experiment-run-wrong", command).code, 75);
    let remote_result = remote.join(".git/oms-remote-ops/experiment-run-wrong/stdout");
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&remote_result).unwrap()).unwrap();
    bundle["run_id"] = "experiment-run-first".into();
    std::fs::write(&remote_result, serde_json::to_vec(&bundle).unwrap()).unwrap();
    let wrong = reconcile("experiment-run-wrong");
    assert_eq!(wrong.code, 3, "{}", String::from_utf8_lossy(&wrong.stderr));
    let wrong: serde_json::Value = serde_json::from_slice(&wrong.stdout).unwrap();
    assert_eq!(wrong["state"], "invalid_result");
    assert_eq!(wrong["metrics_valid"], false);

    // A zero-exit command without its operation-owned sidecar cannot borrow the
    // previous run's metrics. Failure is terminal, never an automatic retry.
    let missing = run("experiment-run-missing", "true");
    assert_ne!(missing.code, 0);
    let missing: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["state"], "failed");
    assert_eq!(missing["metrics_valid"], false);
    let calls = std::fs::read(root.join("calls")).unwrap();
    assert_ne!(run("experiment-run-missing", "true").code, 0);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);

    std::fs::write(
        remote.join(".gitignore"),
        ".oms/\n# remote snapshot drift\n",
    )
    .unwrap();
    let drift = run("experiment-run-drift", command);
    assert_eq!(drift.code, 2, "{}", String::from_utf8_lossy(&drift.stderr));
    let drift: serde_json::Value = serde_json::from_slice(&drift.stdout).unwrap();
    assert_eq!(drift["state"], "failed");
    assert_eq!(drift["failure_kind"], "snapshot_drift");
    assert_eq!(drift["reconcile_required"], false);
    assert!(drift.get("command_exit").is_none());
    let durable: serde_json::Value = serde_json::from_slice(
        &std::fs::read(f.0.join(drift["execution_receipt"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(durable["failure_kind"], "snapshot_drift");
    assert!(durable["remote_meta"].is_null());
    let calls = std::fs::read(root.join("calls")).unwrap();
    let terminal = std::fs::read(&ledger).unwrap();
    assert_eq!(run("experiment-run-drift", command).code, 2);
    assert_eq!(reconcile("experiment-run-drift").code, 2);
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    assert_eq!(std::fs::read(&ledger).unwrap(), terminal);
    assert_eq!(
        std::fs::read_to_string(remote.join(".git/executions")).unwrap(),
        "ran\nran\n"
    );
    let exhausted = run("experiment-run-extra", command);
    assert_eq!(exhausted.code, 2);
    assert!(String::from_utf8_lossy(&exhausted.stderr).contains("run budget spent"));
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    let summary = f.run(&["experiment", "summarize", "--id", "trial"], false);
    assert_eq!(summary.code, 0);
    let summary: serde_json::Value = serde_json::from_slice(&summary.stdout).unwrap();
    assert_eq!(summary["budget"]["runs"]["used"], 4);
    assert_eq!(summary["verdict"], "inconclusive");
}
