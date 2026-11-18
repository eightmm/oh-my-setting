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
