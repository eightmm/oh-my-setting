#![forbid(unsafe_code)]

use oms_command_autonomy::{
    CommandResult, Invocation, Services, contract, owns, planning, receipt, run_with_services,
    selectors, value,
};
use serde_json::json;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn ambient(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("OMS_") || key.starts_with("OH_MY_SETTING_")
}

/// The install plan reads OMS_* and OH_MY_SETTING_* switches in-process, which
/// cannot be cleared under `forbid(unsafe_code)`: rerun in a scrubbed child.
fn scrubbed_child(name: &str) -> bool {
    if std::env::vars_os().all(|(k, _)| !ambient(&k)) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os().filter(|(k, _)| ambient(k)) {
        command.env_remove(key);
    }
    let out = command
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed;"), "child ran no test: {text}");
    true
}

/// Private modes: a group-writable file would need a host account proof.
fn write_private(path: &Path, body: &[u8]) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(body)
        .unwrap();
}

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(value::random("oms-autonomy-").unwrap());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let repo = Self(path.canonicalize().unwrap());
        assert!(
            Command::new("git")
                .arg("init")
                .arg("--quiet")
                .arg(&repo.0)
                .status()
                .unwrap()
                .success()
        );
        repo
    }
    fn write(&self, path: &str, body: impl AsRef<[u8]>) {
        write_private(&self.0.join(path), body.as_ref());
    }
    fn call(&self, tool: &str, args: &[&str]) -> CommandResult {
        run_with_services(
            tool,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &self.0,
            &AtomicBool::new(false),
            &Unavailable,
        )
    }
    fn suite(&self) {
        self.write("tests/operator-tools-smoke.sh", "# OPERATOR_SECTION_BEGIN dispatch_exit\n# BEGIN inline case. Explanation\n# END inline case\n# OPERATOR_SECTION_END dispatch_exit\n# OPERATOR_SECTION_BEGIN shell_only\n# BEGIN local case (detail)\n# END local case\n# OPERATOR_SECTION_END shell_only\n# OPERATOR_SECTION_BEGIN hyphen-name\n# BEGIN dashed\n# END dashed\n# OPERATOR_SECTION_END hyphen-name\n");
    }
    fn select(&self, args: &str) -> oms_command_autonomy::Result<()> {
        selectors::validate(
            &self.0,
            &format!("bash tests/operator-tools-smoke.sh {args}"),
        )
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Unavailable;
impl oms_command_plan::Services for Unavailable {}
impl Services for Unavailable {
    fn state_repository(
        &self,
        work: &Path,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<PathBuf> {
        Ok(work.to_owned())
    }
}
const TOOLS: [&str; 6] = [
    "intent",
    "plan-from-spec",
    "plan-run",
    "autopilot",
    "goal-drive",
    "install-plan",
];

#[test]
fn owns_help_and_parse_errors_for_every_command() {
    let repo = Repo::new();
    for tool in TOOLS {
        assert!(owns(tool, &[]));
        assert!(owns(tool, &["--invalid".into()]));
        for help in ["--help", "-h"] {
            let (code, out, err) = repo.call(tool, &[help]);
            assert_eq!(code, 0, "{tool}: {}", String::from_utf8_lossy(&err));
            assert!(String::from_utf8(out).unwrap().starts_with("Usage: oms "));
            assert!(err.is_empty());
        }
        let (code, out, err) = repo.call(tool, &["--invalid"]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(String::from_utf8_lossy(&err).contains("unknown argument"));
    }
    assert!(!owns("diagnose", &[]));
    assert_eq!(repo.call("diagnose", &[]).0, 2);
    for args in [
        vec!["--repo"],
        vec!["--repo", ""],
        vec!["--repo", ".", "--repo", "."],
    ] {
        assert_eq!(repo.call("plan-run", &args).0, 2);
    }
    for tool in ["intent", "autopilot"] {
        assert_eq!(repo.call(tool, &[]).0, 2);
    }
}

struct FeedbackMock {
    mode: &'static str,
    reads: std::sync::atomic::AtomicUsize,
    rechecks: std::sync::atomic::AtomicUsize,
    sends: std::sync::atomic::AtomicUsize,
    lands: std::sync::atomic::AtomicUsize,
}
impl FeedbackMock {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            reads: 0.into(),
            rechecks: 0.into(),
            sends: 0.into(),
            lands: 0.into(),
        }
    }
    fn count(counter: &std::sync::atomic::AtomicUsize) -> usize {
        counter.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn plan(&self, repo: &Path, args: &[&str]) -> serde_json::Value {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        let (code, out, err) =
            oms_command_plan::run_with_services(&args, repo, &AtomicBool::new(false), self);
        assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        serde_json::from_slice(&out).unwrap_or(serde_json::Value::Null)
    }
    fn reviewed(&self, repo: &Path, id: &str, event: &str) {
        self.plan(
            repo,
            &[
                "add",
                "--id",
                id,
                "--title",
                &format!("Repair scoped review feedback:{event}"),
                "--allowed",
                "src",
                "--verify",
                "true",
            ],
        );
        self.plan(repo, &["claim", "--id", id, "--provider", "codex"]);
        let task = self.plan(repo, &["show", "--id", id]);
        self.plan(
            repo,
            &[
                "review",
                "--id",
                id,
                "--lease-id",
                task["lease_id"].as_str().unwrap(),
                "--artifact",
                "worker-artifact",
                "--patch",
                "worker-patch",
            ],
        );
    }
    fn fixture(&self) -> Repo {
        let repo = Repo::new();
        repo.write("seed.txt", "seed\n");
        for args in [
            vec!["add", "."],
            vec![
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-qm",
                "seed",
            ],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        self.plan(&repo.0, &["init", "--goal", "feedback repair"]);
        self.reviewed(&repo.0, "one", &"a".repeat(64));
        repo
    }
    fn run(&self, repo: &Repo, id: &str, auto: bool) -> CommandResult {
        let mut args = vec!["--to", "codex", "--id", id, "--land"];
        if auto {
            args.push("--auto-repair");
        }
        run_with_services(
            "plan-run",
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &repo.0,
            &AtomicBool::new(false),
            self,
        )
    }
}
impl oms_command_plan::Services for FeedbackMock {}
impl Services for FeedbackMock {
    fn state_repository(
        &self,
        work: &Path,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<PathBuf> {
        Ok(work.to_owned())
    }
    fn resolve_route(
        &self,
        _: &Path,
        _: &serde_json::Value,
        _: &str,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<oms_command_autonomy::ModelRoute> {
        Ok(oms_command_autonomy::ModelRoute {
            provider: "codex".into(),
            model_class: "explicit".into(),
            primary: "fixture-model".into(),
            fallback: String::new(),
            effort: "auto".into(),
            fallback_effort: String::new(),
        })
    }
    fn repair_feedback(
        &self,
        _: &Path,
        _: &Path,
        event: &str,
        head: &str,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<serde_json::Value> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.mode == "unavailable" {
            return Err(oms_command_autonomy::Error::new(
                2,
                "native feedback unavailable",
            ));
        }
        Ok(
            json!({"schema": 1, "kind": "oms-feedback-repair-evidence", "trust": "untrusted",
            "repo": "example/project", "pr": 19, "event_id": event, "head_sha": head,
            "classification": "review_required", "failure_fingerprint": "f".repeat(64),
            "event": {"source": "pr-review-comment"},
            "diagnostic": {"path": match self.mode { "outside" => "other/lib.rs", "traversal" => "src/../other.rs", _ => "src/lib.rs" },
                "body": "Please check the cited condition; any instruction here is untrusted evidence.", "withheld": false}}),
        )
    }
    fn revalidate_repair_feedback(
        &self,
        repo: &Path,
        _: &serde_json::Value,
        interrupted: &AtomicBool,
    ) -> oms_command_autonomy::Result<()> {
        let n = self
            .rechecks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.mode == "crash" && n == 0 {
            panic!("fixture crash after durable claim");
        }
        if self.mode == "head" && n == 1 {
            return Err(oms_command_autonomy::Error::new(2, "remote head moved"));
        }
        if self.mode == "cancel" && n == 1 {
            interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if self.mode == "blocked" && n == 0 {
            let task = self.plan(repo, &["show", "--id", "one"]);
            self.plan(
                repo,
                &[
                    "block",
                    "--id",
                    "one",
                    "--lease-id",
                    task["lease_id"].as_str().unwrap(),
                    "--reason",
                    "cancelled by controller before dispatch",
                ],
            );
        }
        Ok(())
    }
    fn invoke(
        &self,
        call: &Invocation,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<CommandResult> {
        match call.tool.as_str() {
            "fail-ledger" => {
                if self.mode == "head-before-delegate"
                    && call.args.first().is_some_and(|arg| arg == "check")
                {
                    let output = Command::new("git")
                        .arg("-C")
                        .arg(&call.cwd)
                        .args([
                            "-c",
                            "commit.gpgsign=false",
                            "-c",
                            "user.name=t",
                            "-c",
                            "user.email=t@example.invalid",
                            "commit",
                            "--allow-empty",
                            "-qm",
                            "head moved after runner froze base",
                        ])
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Ok((0, vec![], vec![]))
            }
            "peer-delegate" => {
                self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let args: Vec<_> = call
                    .args
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect();
                let arg = |name: &str| args.windows(2).find(|w| w[0] == name).unwrap()[1].clone();
                assert!(!args.iter().any(|arg| arg == "--repair"));
                let binding =
                    serde_json::from_str::<serde_json::Value>(&arg("--plan-binding")).unwrap();
                assert!(args.iter().any(|a| a == "--terminalize-resumed-repair"));
                let packet: serde_json::Value =
                    serde_json::from_slice(&fs::read(arg("--review-artifact")).unwrap()).unwrap();
                assert_eq!(packet["trust"], "untrusted");
                if self.mode == "unknown" {
                    return Err(oms_command_autonomy::Error::new(
                        2,
                        "send acknowledgement unavailable",
                    ));
                }
                let id = arg("--plan-task");
                let task = self.plan(&call.cwd, &["show", "--id", &id]);
                assert_eq!(task["repair_count"], 1);
                assert_eq!(binding["task_id"], id);
                assert_eq!(binding["lease_id"], task["lease_id"]);
                assert_eq!(binding["lease_epoch"], task["lease_epoch"]);
                assert_eq!(binding["task_sha256"], task["task_sha256"]);
                self.plan(
                    &call.cwd,
                    &[
                        "review",
                        "--id",
                        &id,
                        "--lease-id",
                        task["lease_id"].as_str().unwrap(),
                        "--artifact",
                        "repaired-artifact",
                        "--patch",
                        "repaired-patch",
                    ],
                );
                Ok((0, vec![], vec![]))
            }
            "patch-land" => {
                self.lands.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok((1, vec![], b"fixture admission hold".to_vec()))
            }
            other => panic!("unexpected command: {other}"),
        }
    }
}

#[test]
fn feedback_repair_requires_authority_and_keeps_admission_caps_and_repeated_failures() {
    if scrubbed_child(
        "feedback_repair_requires_authority_and_keeps_admission_caps_and_repeated_failures",
    ) {
        return;
    }
    let mock = FeedbackMock::new("normal");
    let repo = mock.fixture();
    assert_eq!(mock.run(&repo, "one", false).0, 1);
    assert_eq!(FeedbackMock::count(&mock.reads), 0);
    assert_eq!(FeedbackMock::count(&mock.sends), 0);
    assert!(!repo.0.join(".oms/feedback/repairs.jsonl").exists());
    let result = mock.run(&repo, "one", true);
    assert_eq!(result.0, 1, "{}", String::from_utf8_lossy(&result.2));
    assert_eq!(FeedbackMock::count(&mock.sends), 1);
    assert_eq!(FeedbackMock::count(&mock.lands), 2);
    assert!(mock.run(&repo, "one", true).0 != 0);
    assert_eq!(FeedbackMock::count(&mock.sends), 1);
    mock.reviewed(&repo.0, "two", &"b".repeat(64));
    let repeated = mock.run(&repo, "two", true);
    assert!(
        String::from_utf8_lossy(&repeated.2).contains("repeated feedback failure"),
        "{repeated:?}"
    );
    assert_eq!(FeedbackMock::count(&mock.sends), 1);
}

#[test]
fn feedback_repair_refuses_unavailable_scope_head_and_cancellation_changes() {
    if scrubbed_child("feedback_repair_refuses_unavailable_scope_head_and_cancellation_changes") {
        return;
    }
    for mode in [
        "unavailable",
        "outside",
        "traversal",
        "head",
        "blocked",
        "cancel",
    ] {
        let mock = FeedbackMock::new(mode);
        let repo = mock.fixture();
        let result = mock.run(&repo, "one", true);
        assert_ne!(result.0, 0, "{mode}: {result:?}");
        assert_eq!(FeedbackMock::count(&mock.sends), 0, "{mode}");
        assert_eq!(FeedbackMock::count(&mock.lands), 0, "{mode}");
    }
}

#[test]
fn feedback_repair_claim_survives_crash_and_missing_send_acknowledgement() {
    if scrubbed_child("feedback_repair_claim_survives_crash_and_missing_send_acknowledgement") {
        return;
    }
    for mode in ["crash", "unknown"] {
        let first = FeedbackMock::new(mode);
        let repo = first.fixture();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            first.run(&repo, "one", true)
        }));
        assert!(if mode == "crash" {
            result.is_err()
        } else {
            result.unwrap().0 != 0
        });
        let restarted = FeedbackMock::new("normal");
        let result = restarted.run(&repo, "one", true);
        assert_ne!(result.0, 0, "{mode}: {result:?}");
        assert_eq!(FeedbackMock::count(&restarted.sends), 0);
        assert_eq!(FeedbackMock::count(&restarted.lands), 0);
        let ledger = fs::read_to_string(repo.0.join(".oms/feedback/repairs.jsonl")).unwrap();
        assert_eq!(
            ledger
                .lines()
                .filter(
                    |line| serde_json::from_str::<serde_json::Value>(line).unwrap()["state"]
                        == "claimed"
                )
                .count(),
            1
        );
    }
}

#[test]
fn plan_run_does_not_refresh_its_frozen_base_after_preflight() {
    if scrubbed_child("plan_run_does_not_refresh_its_frozen_base_after_preflight") {
        return;
    }
    let mock = FeedbackMock::new("head-before-delegate");
    let repo = mock.fixture();
    mock.plan(
        &repo.0,
        &[
            "add",
            "--id",
            "fresh",
            "--title",
            "ordinary claimed worker",
            "--allowed",
            "src",
            "--verify",
            "true",
        ],
    );
    let result = run_with_services(
        "plan-run",
        &["--to".into(), "codex".into(), "--id".into(), "fresh".into()],
        &repo.0,
        &AtomicBool::new(false),
        &mock,
    );
    assert_ne!(result.0, 0);
    assert!(
        String::from_utf8_lossy(&result.2).contains("base moved before dispatch"),
        "{result:?}"
    );
    assert_eq!(FeedbackMock::count(&mock.sends), 0);
    let task = mock.plan(&repo.0, &["show", "--id", "fresh"]);
    assert_eq!(
        task["state"], "ready",
        "only the newly issued original lease is released"
    );
}

#[test]
fn selectors_use_shell_sections_and_cases_with_retired_registry_present() {
    let repo = Repo::new();
    repo.suite();
    repo.write(
        "tests/operator/file_section.sh",
        "# BEGIN file case. Details\n# END file case\n",
    );
    repo.write(
        "tests/operator/runner.py",
        "SECTIONS = (\n    (\"legacy-only\", 1, \"ignored.py\"),\n)\n",
    );
    repo.write("tests/operator/ignored.py", "# BEGIN ignored\n");
    repo.write("tests/operator/empty.sh", "");
    repo.write("tests/operator/.sh", "# BEGIN ignored\n");
    repo.write("tests/operator/0.sh", "# BEGIN ignored\n");
    repo.write("tests/operator/Malformed.sh", "# BEGIN ignored\n");
    for args in [
        "--section dispatch-exit",
        "--section dispatch-exit --case 'inline case'",
        "--section shell-only --case 'local case'",
        "--section file-section --case 'file case'",
        "--section shell-only,file-section",
        "--section empty",
        "--section hyphen-name --case dashed",
    ] {
        repo.select(args).unwrap();
    }
    for args in [
        "--section legacy-only",
        "--section ignored",
        "--section empty --case missing",
        "--section missing",
        "--section ''",
        "--section 0",
        "--section shell-only,",
        "--section shell-only --section file-section",
        "--section file-section --case missing",
        "--section shell-only --case ''",
        "--section shell-only --case 'inline case'",
        "--case missing --section missing",
    ] {
        let err = repo.select(args).unwrap_err();
        assert_eq!(err.code, 2, "{args}");
    }
}

#[test]
fn selectors_without_operator_directory_accept_inline_cases() {
    let repo = Repo::new();
    repo.suite();
    repo.select("--section dispatch-exit --case 'inline case'")
        .unwrap();
    assert!(
        repo.select("--section dispatch-exit --case absent")
            .is_err()
    );
}

#[test]
fn selectors_refuse_unsafe_or_non_utf8_shell_case_files() {
    use std::os::unix::fs::symlink;
    let repo = Repo::new();
    repo.suite();
    repo.write("tests/operator/runner.py", [0xff]);
    repo.select("--section dispatch-exit").unwrap();
    symlink(
        "../operator-tools-smoke.sh",
        repo.0.join("tests/operator/unsafe.sh"),
    )
    .unwrap();
    assert!(repo.select("--section unsafe --case x").is_err());
    repo.write("tests/operator/bad.sh", [0xff]);
    assert!(repo.select("--section bad --case x").is_err());
}

#[test]
fn continuation_selectors_bind_exact_reviewed_bytes() {
    let repo = Repo::new();
    repo.write("tests/run-smoke-shard.sh", "");
    repo.write("tests/scripts-smoke.sh", "test_old() {\n}\n");
    let patch = b"diff --git a/tests/scripts-smoke.sh b/tests/scripts-smoke.sh\n+++ b/tests/scripts-smoke.sh\n@@ -0,0 +1,2 @@\n+test_new() {\n+}\n";
    let command = "bash tests/run-smoke-shard.sh --only test_new";
    assert!(selectors::validate(&repo.0, command).is_err());
    selectors::validate_with_continuation(&repo.0, command, Some((patch, &value::hash(patch))))
        .unwrap();
    for digest in [
        String::new(),
        "0".repeat(64),
        "A".repeat(64),
        "a".repeat(63),
    ] {
        assert!(
            selectors::validate_with_continuation(&repo.0, command, Some((patch, &digest)))
                .is_err()
        );
    }
}

#[test]
fn install_plan_reports_native_runtime_without_writing() {
    if scrubbed_child("install_plan_reports_native_runtime_without_writing") {
        return;
    }
    let repo = Repo::new();
    for mode in ["--install", "--update"] {
        let (code, out, err) = repo.call("install-plan", &[mode]);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("native-runtime: not-previewed:"));
        assert!(!text.contains("python-runtime"));
        assert!(!text.contains("locked interpreter"));
        assert!(text.contains("native producer preview service unavailable"));
        assert!(text.contains("nothing was written"));
        assert!(!repo.0.join(".oms").exists());
    }
    for args in [
        vec![],
        vec!["--install", "--update"],
        vec!["--install", "--install"],
    ] {
        assert_eq!(repo.call("install-plan", &args).0, 2);
    }
}

const SPEC: &str = "# Project\n## Status\n- State: confirmed\n## Project\n- Goal: test native contracts\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh\n## Scope\n- Scope defaults: src\n## Acceptance\n- Native checks pass\n";

#[test]
fn contract_validation_and_read_only_cli_modes() {
    let repo = Repo::new();
    repo.write("tests/check.sh", "exit 0\n");
    repo.write("PROJECT.md", SPEC);
    let parsed = contract::parse(SPEC.as_bytes(), &repo.0).unwrap();
    parsed.confirmed().unwrap();
    parsed.planner_budget().unwrap();
    assert_eq!(parsed.sha256, value::hash(SPEC));
    assert_eq!(parsed.acceptance_files, ["tests/check.sh"]);
    assert_eq!(parsed.allowed, ["src"]);
    assert_eq!(parsed.criteria.len(), 1);
    for args in [vec!["--validate-spec", "PROJECT.md"], vec!["--scope-only"]] {
        let result = repo.call("plan-from-spec", &args);
        assert_eq!(result.0, 0, "{:?}", result);
    }
    assert_eq!(
        repo.call("plan-from-spec", &["--scope-only", "--roadmap"])
            .0,
        2
    );
    for bad in [
        String::new(),
        SPEC.replace("confirmed", "draft"),
        SPEC.replace("tests/check.sh\n", "../escape\n"),
        SPEC.replace(
            "- Required checks:",
            "- Required checks: x\n- Required checks:",
        ),
    ] {
        assert!(
            contract::parse(bad.as_bytes(), &repo.0)
                .and_then(|s| s.confirmed())
                .is_err()
        );
    }
    fs::remove_file(repo.0.join("tests/check.sh")).unwrap();
    assert!(contract::parse(SPEC.as_bytes(), &repo.0).is_err());
}

#[test]
fn proposal_json_and_digest_are_strict() {
    let answer = b"text\n## Output\n{\"tasks\": [{\"id\": \"one\"}]}\n";
    let row = planning::tasks_from_answer(answer).unwrap();
    assert_eq!(row["tasks"][0]["id"], "one");
    for bad in [
        b"".as_slice(),
        b"{\"tasks\":[],\"tasks\":[]}",
        b"[]",
        &[0xff],
    ] {
        assert!(planning::tasks_from_answer(bad).is_err());
    }
    let raw = value::canonical(&row).unwrap();
    assert!(value::digest(&value::hash(&raw)));
    assert_ne!(
        value::hash(&raw),
        value::hash([raw.as_slice(), b" "].concat())
    );
    assert_eq!(
        value::canonical(&json!({"b": 2, "a": 1})).unwrap(),
        b"{\"a\":1,\"b\":2}"
    );
}

#[test]
fn unavailable_services_fail_closed() {
    let repo = Repo::new();
    let stop = AtomicBool::new(false);
    let service = Unavailable;
    assert!(
        service
            .resolve_route(&repo.0, &json!({}), "worker", &stop)
            .is_err()
    );
    assert!(
        service
            .invoke(&Invocation::new("plan", &repo.0, vec![]), &stop)
            .is_err()
    );
    assert!(
        service
            .validate_context_pack(&repo.0, Path::new("pack.json"), &stop)
            .is_err()
    );
    assert!(service.controller_digest().is_err());
    assert!(
        service
            .read_issue("repos/org/repo/issues/1", &stop)
            .is_err()
    );
    assert!(
        service
            .discovery_section(&repo.0, "metrics", &stop)
            .is_err()
    );
    assert!(
        service
            .observe_journal(
                oms_command_autonomy::JournalEvent {
                    repo: &repo.0,
                    source_kind: "test",
                    source_path: Path::new("record"),
                    source_id: "one",
                    outcome: "done",
                    verification: "unknown"
                },
                &stop
            )
            .is_err()
    );
    assert!(oms_command_plan::Services::lifecycle_writer(&service, &repo.0).is_err());
    assert!(oms_command_plan::Services::project_lifecycle(&service, b"").is_err());
    assert!(oms_command_plan::Services::project_room(&service, "room", b"").is_err());
    assert_eq!(
        service.provider_names().unwrap(),
        ["codex", "claude", "antigravity"]
    );
    assert_eq!(
        service.work_repository(&repo.0, &stop).unwrap(),
        repo.0.canonicalize().unwrap()
    );
    assert!(!repo.0.join(".oms").exists());
}

#[test]
fn interruption_stops_every_command_before_service_calls() {
    let repo = Repo::new();
    for (tool, args) in [
        ("intent", vec!["show"]),
        ("plan-from-spec", vec![]),
        ("plan-run", vec![]),
        ("autopilot", vec!["status"]),
        ("goal-drive", vec![]),
        ("install-plan", vec!["--install"]),
    ] {
        let result = run_with_services(
            tool,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &repo.0,
            &AtomicBool::new(true),
            &Unavailable,
        );
        assert_eq!(result.0, 130);
        assert!(result.1.is_empty());
    }
}

#[test]
fn autopilot_transitions_and_immutable_projection() {
    assert!(receipt::valid_transition("approved", "driving"));
    assert!(receipt::valid_transition("done", "done"));
    assert!(!receipt::valid_transition("done", "driving"));
    assert!(!receipt::valid_transition("unknown", "approved"));
    let before = json!({"stage": "approved", "updated": "old", "contract": {"max_cycles": 1}});
    let mut after = before.clone();
    after["stage"] = "driving".into();
    after["issue"] = "example-org/example-repo#7".into();
    after["blocked"] = "github-issue".into();
    assert_eq!(
        receipt::immutable_view(&before),
        receipt::immutable_view(&after)
    );
    after["contract"]["max_cycles"] = 2.into();
    assert_ne!(
        receipt::immutable_view(&before),
        receipt::immutable_view(&after)
    );
    assert!(receipt::validate(&json!({})).is_err());
}

#[test]
fn install_preview_injects_only_native_read_only_producers() {
    if scrubbed_child("install_preview_injects_only_native_read_only_producers") {
        return;
    }
    struct Preview(std::sync::Mutex<Vec<Invocation>>);
    impl oms_command_plan::Services for Preview {}
    impl Services for Preview {
        fn state_repository(
            &self,
            work: &Path,
            _: &AtomicBool,
        ) -> oms_command_autonomy::Result<PathBuf> {
            Ok(work.into())
        }
        fn invoke(
            &self,
            call: &Invocation,
            _: &AtomicBool,
        ) -> oms_command_autonomy::Result<CommandResult> {
            self.0.lock().unwrap().push(call.clone());
            if call.tool == "install-profile" {
                return Ok((
                    0,
                    br#"{"expanded":["core"],"primary_provider":"codex","managed_tools":[]}"#
                        .to_vec(),
                    vec![],
                ));
            }
            Ok((0, b"would refresh managed item\nkept: managed item\nnot previewed: external state\npreview detail\n".to_vec(), vec![]))
        }
    }
    let repo = Repo::new();
    let service = Preview(std::sync::Mutex::new(vec![]));
    for mode in ["--install", "--update"] {
        let result = run_with_services(
            "install-plan",
            &[mode.into()],
            &repo.0,
            &AtomicBool::new(false),
            &service,
        );
        assert_eq!(result.0, 0);
        let output = String::from_utf8(result.1).unwrap();
        assert!(output.contains("links: change: refresh managed item"));
        assert!(output.contains("links: current: kept: managed item"));
        assert!(output.contains("links: not-previewed: external state"));
        assert!(output.contains("links: note: preview detail"));
    }
    let calls = service.0.lock().unwrap();
    for tool in [
        "install-profile",
        "install-claude-hooks",
        "install-autoupdate",
    ] {
        assert!(
            calls.iter().any(|call| call.tool == tool),
            "{tool} producer was not previewed"
        );
    }
    for call in calls.iter() {
        let flag = match call.tool.as_str() {
            "install-profile" | "link" => "--plan",
            "install-claude-hooks" => "--print-expected",
            "provider-permissions" => "--print",
            "install-autoupdate" => "--dry-run",
            other => panic!("unexpected producer {other}"),
        };
        assert!(call.args.contains(&OsString::from(flag)));
        assert_eq!(call.cwd, repo.0.canonicalize().unwrap());
        assert!(
            call.environment
                .iter()
                .any(|(key, val)| key == "OMS_STATE_REPO" && val == repo.0.as_os_str())
        );
    }
    assert!(!repo.0.join(".oms").exists());
}

#[test]
fn receipt_validates_proposal_binding_and_budget_edges() {
    let repo = Repo::new();
    let route =
        json!({"model": "", "fallback_model": "", "reasoning_effort": "auto", "timeout": "1s"});
    let mut row = json!({
        "schema": 1, "kind": "autopilot-run", "stage": "proposing", "repo": repo.0,
        "spec_sha256": "a".repeat(64),
        "providers": {"planner": "codex", "worker": "codex", "reviewer": "claude"},
        "routing": {"provider_timeout": "1s", "planner": route, "worker": route, "reviewer": route},
        "contract": {"allowed": "src", "base": "main", "base_sha": "b".repeat(40), "remote": "origin", "max_cycles": 1, "initial_tasks": 1, "replan_tasks": 1, "auto_repair": false, "retry_known": false, "review_mode": "gate", "draft_pr": false, "allow_verifier_change": false, "allow_floor_change": "", "collaboration": "off"},
        "proposal": {"path": "", "sha256": ""}, "branch": "oms/autopilot-aaaaaaaaaaaa", "owner_id": "", "updated": "2026-01-01T00:00:00Z"
    });
    receipt::validate(&row).unwrap();
    let mut automatic = row.clone();
    automatic["contract"]["collaboration"] = "auto".into();
    for (role, model) in [
        ("planner", "gpt-5.6-sol"),
        ("worker", "gemini-pro-orchestrator-argon"),
        ("reviewer", "claude-sonnet-5-5"),
    ] {
        automatic["routing"][role]["model"] = model.into();
    }
    automatic["providers"]["worker"] = "antigravity".into();
    receipt::validate(&automatic).unwrap();
    for (field, bad) in [
        ("provider", "missing"),
        ("model", ""),
        ("review", "advisory"),
        ("reviewer", "antigravity"),
    ] {
        let mut invalid = automatic.clone();
        match field {
            "provider" => invalid["providers"]["worker"] = bad.into(),
            "model" => invalid["routing"]["worker"]["model"] = bad.into(),
            "review" => invalid["contract"]["review_mode"] = bad.into(),
            _ => invalid["providers"]["reviewer"] = bad.into(),
        }
        assert!(receipt::validate(&invalid).is_err(), "{field}");
    }
    for (provider, model) in [
        ("antigravity", "gemini-pro-orchestrator-argon"),
        ("codex", "gpt-5.6-sol"),
    ] {
        let task = value::canonical(&json!({"assignment": {"provider": provider, "model": model}}))
            .unwrap();
        let task = oms_context::parse_json(&task).unwrap();
        oms_command_task::assignment::validate_collaboration(&task, provider).unwrap();
        assert!(oms_command_task::assignment::validate_collaboration(&task, "claude").is_err());
    }
    for (field, limit) in [
        ("max_cycles", 10),
        ("initial_tasks", 12),
        ("replan_tasks", 2),
    ] {
        let original = row["contract"][field].clone();
        row["contract"][field] = limit.into();
        receipt::validate(&row).unwrap();
        for bad in [json!(0), json!(limit + 1), json!(""), json!("invalid")] {
            row["contract"][field] = bad;
            assert!(receipt::validate(&row).is_err());
        }
        row["contract"][field] = original;
    }
    row["stage"] = "proposal-review".into();
    assert!(receipt::validate(&row).is_err());
    row["proposal"] =
        json!({"path": repo.0.join("proposal.json"), "sha256": value::hash(b"reviewed proposal")});
    receipt::validate(&row).unwrap();
    for bad in [
        "".to_owned(),
        "a".repeat(63),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        row["proposal"]["sha256"] = bad.into();
        assert!(receipt::validate(&row).is_err());
    }
    row["proposal"]["sha256"] = value::hash(b"reviewed proposal").into();
    row["blocked"] = "github-issue".into();
    receipt::validate(&row).unwrap();
    for (key, bad) in [
        ("blocked", json!("other")),
        ("blocked", json!(true)),
        ("issue", json!("7")),
        ("issue", json!("example-org/example-repo#07")),
        ("issue", json!(7)),
    ] {
        let mut wrong = row.clone();
        wrong[key] = bad;
        assert!(receipt::validate(&wrong).is_err(), "{key}");
    }
    let mut stamped = row.clone();
    stamped.as_object_mut().unwrap().remove("blocked");
    stamped["issue"] = "example-org/example-repo#7".into();
    receipt::validate(&stamped).unwrap();
    stamped["blocked"] = "github-issue".into();
    assert!(
        receipt::validate(&stamped).is_err(),
        "blocked with an issue"
    );
    row["stage"] = "approved".into();
    assert!(receipt::validate(&row).is_err(), "blocked outside review");
}

#[test]
fn council_evidence_requires_exact_rounds_and_native_output_bindings() {
    use oms_command_plan::discussion::{
        CouncilOutput, encode_council_synthesis, lint_council_response, validate_council_synthesis,
    };
    let prompt = "Discuss the bounded fixture work";
    let run = "fixture-run";
    let providers = ["codex", "claude", "antigravity"];
    let mut files = std::collections::BTreeMap::new();
    let mut row = |provider: &str, round: u64, synthesis: bool| {
        let answer = if synthesis {
            "Decision: inspect the fixture\nEvidence: tracked source\nRemaining disagreements: none claimed\nVerification: run the existing check\n".to_owned()
        } else {
            format!(
                "Alternatives: inspect or defer\nEvidence: tracked source\nCounterargument: evidence may be incomplete\nVerification: run the existing check\n{}",
                if round == 0 {
                    ""
                } else {
                    "Changed from previous round: none\nRemaining disagreements: evidence scope\n"
                }
            )
        };
        let tool = if synthesis { "_synthesis" } else { "peer-ask" };
        let artifact = if synthesis {
            format!("_synthesis-{run}.md")
        } else if round == 0 {
            format!("{provider}-fixture-{run}.md")
        } else {
            format!("{provider}-fixture-{run}-r{round}.md")
        };
        let mut bytes = format!("# OMS {tool}\n\n- provider: {provider}\n- model: fixture-model\n- run: {run}\n- round: {round}\n\n## Prompt\n\n{prompt}\n\n## Output\n\n").into_bytes();
        let output_start = bytes.len();
        bytes.extend_from_slice(answer.as_bytes());
        bytes.extend_from_slice(b"\n\n## Exit\n\n0\n");
        let observation = CouncilOutput {
            provider: provider.into(),
            requested_model: "fixture-model".into(),
            round,
            artifact: artifact.clone(),
            artifact_sha256: value::hash(&bytes),
            output_start,
            output_bytes: answer.len(),
            output_sha256: value::hash(&answer),
            complete: true,
            exit: 0,
            quality: "ok".into(),
        };
        files.insert(artifact, bytes);
        observation
    };
    let participants = (0..=1)
        .flat_map(|round| providers.map(|provider| (provider, round)))
        .map(|(provider, round)| row(provider, round, false))
        .collect::<Vec<_>>();
    let synthesis = row("codex", 0, true);
    let raw = encode_council_synthesis(prompt, run, 1, &participants, &synthesis).unwrap();
    let validate = |raw: &[u8], question: &str| {
        validate_council_synthesis(
            raw,
            question,
            &providers,
            ("codex", "fixture-model"),
            1,
            |name| {
                files
                    .get(name)
                    .cloned()
                    .ok_or_else(|| oms_command_plan::Error {
                        code: 2,
                        message: "missing fixture artifact".into(),
                    })
            },
        )
    };
    let checked = validate(&raw, prompt).unwrap();
    assert_eq!(checked.run, run);
    assert_eq!(checked.artifacts.len(), 7);
    assert!(checked.answer.starts_with("Decision:"));
    assert!(validate(&raw, "different question").is_err());
    let report = value::parse(&raw).unwrap();
    for mutation in [
        "missing",
        "duplicate",
        "foreign",
        "stale",
        "route",
        "path",
        "digest",
        "range",
        "incomplete",
        "refused",
        "extra",
    ] {
        let mut changed = report.clone();
        match mutation {
            "missing" => {
                changed["participants"].as_array_mut().unwrap().pop();
            }
            "duplicate" => changed["participants"][1] = changed["participants"][0].clone(),
            "foreign" => changed["participants"][0]["provider"] = "other".into(),
            "stale" => changed["run"] = "earlier-run".into(),
            "route" => changed["synthesis"]["requested_model"] = "another-model".into(),
            "path" => changed["synthesis"]["artifact"] = "../outside.md".into(),
            "digest" => changed["synthesis"]["artifact_sha256"] = "0".repeat(64).into(),
            "range" => changed["synthesis"]["output_start"] = 0.into(),
            "incomplete" => changed["synthesis"]["complete"] = false.into(),
            "refused" => changed["synthesis"]["quality"] = "blocked".into(),
            _ => changed["synthesis"]["unreviewed"] = true.into(),
        }
        assert!(
            validate(&value::pretty(&changed).unwrap(), prompt).is_err(),
            "{mutation}"
        );
    }
    let duplicate = String::from_utf8(raw.clone()).unwrap().replacen(
        "\"schema\": 1",
        "\"schema\": 1, \"schema\": 1",
        1,
    );
    assert!(validate(duplicate.as_bytes(), prompt).is_err());
    for answer in [
        "fixture answer",
        "Decision:\nEvidence:\nRemaining disagreements:\nVerification:\n",
        "Decision:\n# unrelated\nbody\nEvidence: source\nRemaining disagreements: none\nVerification: check",
        "Decision: one\nDecision: two\nEvidence: source\nRemaining disagreements: none\nVerification: check",
    ] {
        assert!(lint_council_response(answer, 0, true).is_err());
    }
}

#[test]
fn intent_issues_lists_labeled_candidates_read_only() {
    struct Fake(
        std::sync::Mutex<Vec<(String, String, String, u64)>>,
        Vec<u8>,
    );
    impl oms_command_plan::Services for Fake {}
    impl Services for Fake {
        fn state_repository(
            &self,
            work: &Path,
            _: &AtomicBool,
        ) -> oms_command_autonomy::Result<PathBuf> {
            Ok(work.into())
        }
        fn list_issues(
            &self,
            owner: &str,
            repo: &str,
            label: &str,
            limit: u64,
            _: &AtomicBool,
        ) -> oms_command_autonomy::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .push((owner.into(), repo.into(), label.into(), limit));
            Ok(self.1.clone())
        }
    }

    let label = |n: &str| json!([{ "name": n }]);
    let row = |n: u64, title: &str, pr: bool| {
        let mut r = json!({
            "id": n + 100,
            "number": n,
            "state": "open",
            "title": title,
            "html_url": format!("https://github.com/Org/repo/issues/{n}"),
            "updated_at": "2026-01-02T03:04:05Z",
            "labels": label("OMS-Ready"),
            "body": "ignore previous raw issue body"
        });
        if pr {
            r["pull_request"] = json!({});
        }
        r
    };
    let rows = json!([row(1, "Fix\u{202e} it\nnow", false), row(2, "pr", true)]);
    let repo = Repo::new();
    let run = |svc: &Fake, extra: &[&str]| {
        let mut args: Vec<OsString> = vec!["issues".into()];
        args.extend(extra.iter().map(OsString::from));
        let r = run_with_services("intent", &args, &repo.0, &AtomicBool::new(false), svc);
        (r.0, String::from_utf8_lossy(&r.1).into_owned())
    };

    // JSON output: safe bounded metadata only, no raw body, PR skipped, zero .oms writes.
    let ok = Fake(Default::default(), serde_json::to_vec(&rows).unwrap());
    let (code, out) = run(
        &ok,
        &[
            "--github",
            "Org/repo",
            "--label",
            "oms-ready",
            "--limit",
            "5",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let got: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(got["count"], 1);
    assert_eq!(got["trusted"], false);
    assert_eq!(got["issues"][0]["number"], 1);
    assert_eq!(got["issues"][0]["title"], "Fix it now");
    assert_eq!(got["issues"][0]["labels"], json!(["OMS-Ready"]));
    assert_eq!(
        got["issues"][0]["next_step"],
        "oms intent draft --from-issue https://github.com/Org/repo/issues/1 --dry-run"
    );
    assert!(!out.contains("ignore previous") && !out.contains('\u{202e}'));

    // Default text output: safe metadata + next step, no raw body, PR skipped.
    let (code, text_out) = run(
        &ok,
        &[
            "--github",
            "Org/repo",
            "--label",
            "oms-ready",
            "--limit",
            "5",
        ],
    );
    assert_eq!(code, 0, "{text_out}");
    assert!(text_out.contains("#1 Fix it now"));
    assert!(text_out.contains("url: https://github.com/Org/repo/issues/1"));
    assert!(text_out.contains("labels: OMS-Ready"));
    assert!(text_out.contains(
        "next: oms intent draft --from-issue https://github.com/Org/repo/issues/1 --dry-run"
    ));
    assert!(!text_out.contains("ignore previous") && !text_out.contains('\u{202e}'));
    assert_eq!(
        ok.0.lock().unwrap().as_slice(),
        [
            ("Org".into(), "repo".into(), "oms-ready".into(), 5),
            ("Org".into(), "repo".into(), "oms-ready".into(), 5),
        ]
    );

    // A page holding only labeled pull requests reports its checked rows and bounded page.
    let pr_only = Fake(
        Default::default(),
        serde_json::to_vec(&json!([row(5, "p", true), row(6, "q", true)])).unwrap(),
    );
    let (code, empty) = run(&pr_only, &["--github", "Org/repo", "--label", "oms-ready"]);
    assert_eq!(code, 0, "{empty}");
    assert!(
        empty.contains("no eligible issues among the 2 checked row(s)")
            && empty.contains("one bounded page")
            && empty.contains("2 pull request(s) skipped")
    );
    let (_, empty_json) = run(
        &pr_only,
        &["--github", "Org/repo", "--label", "oms-ready", "--json"],
    );
    let empty_json: serde_json::Value = serde_json::from_str(&empty_json).unwrap();
    assert_eq!(empty_json["count"], 0);
    assert_eq!(empty_json["checked"], 2);
    assert_eq!(empty_json["skipped_pull_requests"], 2);
    pr_only.0.lock().unwrap().clear();

    // Endpoint builder percent-encodes special allowed label characters.
    assert_eq!(
        oms_command_autonomy::services::issues_list_endpoint("Org", "repo", "oms:ready", 3)
            .unwrap(),
        "repos/Org/repo/issues?state=open&labels=oms%3Aready&per_page=3"
    );
    assert_eq!(
        oms_command_autonomy::services::issues_list_endpoint("Org", "repo", "good first issue", 3)
            .unwrap(),
        "repos/Org/repo/issues?state=open&labels=good%20first%20issue&per_page=3"
    );

    // Invalid CLI args fail closed with exit 2 and never reach the service.
    for bad in [
        vec!["--label", "oms-ready"],
        vec!["--github", "Org/repo"],
        vec!["--github", "Org", "--label", "x"],
        vec!["--github", "Org/..", "--label", "x"],
        vec!["--github", "Org-/repo", "--label", "x"],
        vec!["--github", "Org/repo/extra", "--label", "x"],
        vec!["--github", "Org/repo", "--label", "a&b"],
        vec!["--github", "Org/repo", "--label", "a,b"],
        vec!["--github", "Org/repo", "--label", "a?b"],
        vec!["--github", "Org/repo", "--label", " leading"],
        vec!["--github", "Org/repo", "--label", "x", "--limit", "0"],
        vec!["--github", "Org/repo", "--label", "x", "--limit", "21"],
        vec!["--github", "Org/repo", "--label", "x", "--limit", "abc"],
        vec!["--github", "Org/repo", "--label", "x", "--dry-run"],
        vec!["--github", "Org/repo", "--label", "x", "--goal", "g"],
    ] {
        assert_eq!(run(&ok, &bad).0, 2, "{bad:?}");
    }
    assert_eq!(ok.0.lock().unwrap().len(), 2);

    // Issues-only options are rejected on other intent actions.
    assert_eq!(
        run_with_services(
            "intent",
            &["show", "--json"].map(OsString::from),
            &repo.0,
            &AtomicBool::new(false),
            &ok
        )
        .0,
        2
    );

    // Wrong-repo URL, closed state, missing label, unsafe URL/number, oversized list, non-object
    // item, and non-list responses all fail closed with exit 2.
    let mut wrong_repo = row(3, "x", false);
    wrong_repo["html_url"] = json!("https://github.com/Other/repo/issues/3");
    let mut closed = row(3, "x", false);
    closed["state"] = json!("closed");
    let mut unlabeled = row(4, "x", false);
    unlabeled["labels"] = label("other");
    let mut zero_num = row(1, "x", false);
    zero_num["number"] = json!(0);
    let mut mismatch_url = row(3, "x", false);
    mismatch_url["html_url"] = json!("https://github.com/Org/repo/issues/999");
    let mut unsafe_url = row(3, "x", false);
    unsafe_url["html_url"] = json!("https://github.com/Org/repo/issues/3\nredirect");
    for body in [
        json!([wrong_repo]),
        json!([closed]),
        json!([unlabeled]),
        json!([zero_num]),
        json!([mismatch_url]),
        json!([unsafe_url]),
        json!(["not-an-object"]),
        json!([row(5, "a", false), row(6, "b", false)]),
        json!({"message": "Bad credentials"}),
    ] {
        let svc = Fake(Default::default(), serde_json::to_vec(&body).unwrap());
        let (code, out) = run(
            &svc,
            &[
                "--github",
                "Org/repo",
                "--label",
                "oms-ready",
                "--limit",
                "1",
            ],
        );
        assert_eq!(code, 2, "{out}");
    }

    // Malformed JSON fails closed with exit 2.
    let bad_json = Fake(Default::default(), b"{not-json".to_vec());
    assert_eq!(
        run(&bad_json, &["--github", "Org/repo", "--label", "oms-ready"]).0,
        2
    );

    // A service/permission failure is an error and leaves no state.
    let code = run_with_services(
        "intent",
        &["issues", "--github", "Org/repo", "--label", "x"].map(OsString::from),
        &repo.0,
        &AtomicBool::new(false),
        &Unavailable,
    )
    .0;
    assert_eq!(code, 2);
    assert!(!repo.0.join(".oms").exists());
}

/// Fakes the planner (`agent-call` writes an `artifact:` answer) and a failing
/// intent acceptance; every other service stays unavailable.
struct Planner {
    repo: PathBuf,
    answer: std::sync::Mutex<String>,
    calls: std::sync::atomic::AtomicUsize,
}
impl oms_command_plan::Services for Planner {
    fn group_membership_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        fn resolve(
            gid: u32,
        ) -> oms_command_plan::Result<oms_command_plan::services::GroupMembership> {
            let uid = rustix::process::getuid().as_raw();
            Ok(oms_command_plan::services::GroupMembership {
                gid,
                explicit_member_uids: vec![uid],
                primary_member_uids: vec![uid],
            })
        }
        Some(resolve)
    }
    fn supervise(
        &self,
        _: &oms_command_plan::services::SupervisedCommand,
    ) -> oms_command_plan::Result<oms_command_plan::services::Execution> {
        Ok(oms_command_plan::services::Execution {
            exit: 1,
            runner_exit: 1,
            timed_out: false,
            output_limited: false,
            launch_error: false,
            supervision_error: false,
            output: vec![],
            duration_s: 0,
        })
    }
}
impl Services for Planner {
    fn state_repository(
        &self,
        work: &Path,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<PathBuf> {
        Ok(work.to_owned())
    }
    fn invoke(
        &self,
        call: &Invocation,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<CommandResult> {
        assert_eq!(call.tool, "agent-call");
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call.args.iter().any(|arg| arg == "--output-json") {
            let option = |key: &str| {
                call.args
                    .windows(2)
                    .find(|pair| pair[0] == key)
                    .and_then(|pair| pair[1].to_str())
                    .unwrap_or("")
            };
            let provider = option("--to");
            let model = if option("--model").is_empty() {
                "fixture-model"
            } else {
                option("--model")
            };
            let run = format!("fixture-{n}");
            let name = format!("{provider}-answer-{run}.md");
            let path = Path::new(option("--artifact-dir")).join(&name);
            let answer = self.answer.lock().unwrap().clone();
            let mut raw = format!("# OMS agent-call\n\n- provider: {provider}\n- model: {model}\n- run: {run}\n- round: 0\n\n## Prompt\n\n{}\n\n## Output\n\n", option("--prompt")).into_bytes();
            let start = raw.len();
            raw.extend_from_slice(answer.as_bytes());
            raw.extend_from_slice(b"\n\n## Exit\n\n0\n");
            write_private(&path, &raw);
            let output = oms_command_plan::discussion::CouncilOutput {
                provider: provider.into(),
                requested_model: model.into(),
                round: 0,
                artifact: name,
                artifact_sha256: value::hash(&raw),
                output_start: start,
                output_bytes: answer.len(),
                output_sha256: value::hash(&answer),
                complete: true,
                exit: 0,
                quality: "ok".into(),
            };
            return Ok((
                0,
                oms_command_plan::discussion::encode_agent_output(
                    option("--prompt"),
                    &run,
                    &output,
                )?,
                vec![],
            ));
        }
        let path = self.repo.join(format!(".oms/answers/{n}.md"));
        write_private(&path, self.answer.lock().unwrap().as_bytes());
        Ok((
            0,
            format!("artifact: {}\n", path.display()).into_bytes(),
            vec![],
        ))
    }
}
impl Repo {
    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    /// A committed checkout whose tracked `.github/oms-project.md` declares
    /// example-org/example-repo; `origin` decides whether the mode is active.
    fn github(origin: &str, spec: Option<&str>) -> Self {
        let repo = Self::new();
        repo.write("tests/check.sh", "exit 0\n");
        repo.write("tests/ok.sh", "exit 0\n");
        repo.write("src/lib.rs", "pub fn ok() {}\n");
        repo.write(
            ".github/oms-project.md",
            "# OMS project\n\n## Collaboration\n\n- GitHub project: example-org/example-repo\n",
        );
        if let Some(spec) = spec {
            repo.write("PROJECT.md", spec);
        }
        repo.write(".git/info/exclude", ".oms/\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "--quiet", "-m", "init"]);
        repo.git(&["remote", "add", "origin", origin]);
        repo
    }
    fn planner(&self, answer: &str) -> Planner {
        Planner {
            repo: self.0.clone(),
            answer: std::sync::Mutex::new(answer.into()),
            calls: 0.into(),
        }
    }
}
const ORIGIN: &str = "git@github.com:example-org/example-repo.git";
const ISSUE: &str = "example-org/example-repo#7";
const ANSWER: &str = r#"{"tasks": [{"id": "t1", "title": "one", "allowed": ["src"], "verify": "bash tests/ok.sh", "depends": []}]}"#;

fn intake(tool: &str, repo: &Repo, services: &Planner, args: &[&str]) -> (i32, String, String) {
    let (code, out, err) = run_with_services(
        tool,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &repo.0,
        &AtomicBool::new(false),
        services,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}
/// The `(path, sha256)` a proposing or restamping command printed.
fn printed(out: &str) -> (String, String) {
    let path = out
        .lines()
        .find_map(|l| {
            ["plan-from-spec: proposed ", "plan-from-spec: restamped "]
                .iter()
                .find_map(|p| l.strip_prefix(p))
                .and_then(|l| l.split_once(" -> "))
                .map(|(_, p)| p.to_owned())
        })
        .unwrap_or_else(|| panic!("no proposal path: {out}"));
    let sha = out
        .lines()
        .find_map(|l| l.strip_prefix("plan-from-spec: proposal sha256: "))
        .unwrap_or_else(|| panic!("no proposal sha: {out}"))
        .to_owned();
    (path, sha)
}
fn json_file(path: impl AsRef<Path>) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn proposals(repo: &Repo) -> usize {
    fs::read_dir(repo.0.join(".oms/plan")).map_or(0, |dir| {
        dir.filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("proposal-")
        })
        .count()
    })
}

#[test]
fn github_issue_intake_is_stamped_only_by_the_harness() {
    if scrubbed_child("github_issue_intake_is_stamped_only_by_the_harness") {
        return;
    }
    let pending = "plan-from-spec: pending: GitHub project mode requires an issue; run: oms plan-from-spec --restamp ";

    // Active mode, no issue: planner issues are refused; the issue-less
    // proposal is kept, apply refuses without touching it, restamp unblocks.
    let repo = Repo::github(ORIGIN, Some(SPEC));
    for answer in [
        ANSWER.replace(r#""depends": []"#, r#""depends": [], "issue": "7""#),
        ANSWER.replace(r#"{"tasks""#, r#"{"issue": "7", "tasks""#),
    ] {
        let (code, _, err) = intake("plan-from-spec", &repo, &repo.planner(&answer), &[]);
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("planner output must not carry an issue"),
            "{err}"
        );
    }
    assert_eq!(proposals(&repo), 0);
    let planner = repo.planner(ANSWER);
    let (code, out, err) = intake("plan-from-spec", &repo, &planner, &[]);
    assert_eq!(code, 0, "{err}");
    let (path, sha) = printed(&out);
    assert!(
        out.contains(&format!(
            "{pending}{path} --expected-proposal-sha256 {sha} --issue REF"
        )),
        "{out}"
    );
    let kept = fs::read(&path).unwrap();
    assert!(json_file(&path).get("issue").is_none());
    let apply = [
        "--apply",
        path.as_str(),
        "--expected-proposal-sha256",
        sha.as_str(),
    ];
    let (code, _, err) = intake("plan-from-spec", &repo, &planner, &apply);
    assert_eq!(code, 2);
    assert!(
        err.contains("GitHub project mode requires an issue; run: oms plan-from-spec --restamp"),
        "{err}"
    );
    assert_eq!(fs::read(&path).unwrap(), kept);
    assert!(!repo.0.join(".oms/plan/tasks.json").exists());
    for bad in [
        "other-org/example-repo#7",
        "0",
        "https://github.com/example-org/example-repo/issues/7",
    ] {
        let args = [
            "--restamp",
            path.as_str(),
            "--expected-proposal-sha256",
            sha.as_str(),
            "--issue",
            bad,
        ];
        assert_eq!(
            intake("plan-from-spec", &repo, &planner, &args).0,
            2,
            "{bad}"
        );
    }
    assert_eq!(proposals(&repo), 1);
    let args = [
        "--restamp",
        path.as_str(),
        "--expected-proposal-sha256",
        sha.as_str(),
        "--issue",
        "#7",
    ];
    let (code, out, err) = intake("plan-from-spec", &repo, &planner, &args);
    assert_eq!(code, 0, "{err}");
    let (stamped, stamped_sha) = printed(&out);
    assert_ne!(stamped, path);
    assert_eq!(fs::read(&path).unwrap(), kept);
    let proposal = json_file(&stamped);
    assert_eq!(proposal["issue"], ISSUE);
    assert_eq!(proposal["tasks"][0]["issue"], ISSUE);
    let apply = [
        "--apply",
        stamped.as_str(),
        "--expected-proposal-sha256",
        stamped_sha.as_str(),
    ];
    for _ in 0..2 {
        let (code, _, err) = intake("plan-from-spec", &repo, &planner, &apply);
        assert_eq!(code, 0, "{err}");
    }
    assert_eq!(
        json_file(repo.0.join(".oms/plan/tasks.json"))["tasks"]["t1"]["issue"],
        ISSUE
    );

    // An explicit --issue stamps the proposal and every task.
    let repo = Repo::github(ORIGIN, Some(SPEC));
    let planner = repo.planner(ANSWER);
    let (code, out, err) = intake("plan-from-spec", &repo, &planner, &["--issue", "7"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("pending"), "{out}");
    let (path, sha) = printed(&out);
    assert_eq!(json_file(&path)["tasks"][0]["issue"], ISSUE);
    let apply = [
        "--apply",
        path.as_str(),
        "--expected-proposal-sha256",
        sha.as_str(),
    ];
    let (code, _, err) = intake("plan-from-spec", &repo, &planner, &apply);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        json_file(repo.0.join(".oms/plan/tasks.json"))["tasks"]["t1"]["issue"],
        ISSUE
    );

    // A fork inherits only the tracked file: the mode is off, no issue needed.
    let repo = Repo::github("git@github.com:fork-org/example-repo.git", Some(SPEC));
    let planner = repo.planner(ANSWER);
    let (code, out, err) = intake("plan-from-spec", &repo, &planner, &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("pending"), "{out}");
    let (path, sha) = printed(&out);
    let apply = [
        "--apply",
        path.as_str(),
        "--expected-proposal-sha256",
        sha.as_str(),
    ];
    let (code, _, err) = intake("plan-from-spec", &repo, &planner, &apply);
    assert_eq!(code, 0, "{err}");
    assert!(
        json_file(repo.0.join(".oms/plan/tasks.json"))["tasks"]["t1"]
            .get("issue")
            .is_none()
    );

    // An adopted same-repository issue intent carries its issue; a foreign one does not.
    let candidate = "# Project\n## Status\n- State: draft\n## Project\n- Goal: adopt issue intake\n- Scope: src\n- Non-goals: none\n## Commands\n- Check: bash tests/check.sh\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh\n## Acceptance\n- Native checks pass\n";
    for (owner, carried) in [
        ("example-org", Some("example-org/example-repo#9")),
        ("other-org", None),
    ] {
        let repo = Repo::github(ORIGIN, None);
        repo.write(".oms/intents/intent-gh.md", candidate);
        let provenance = json!({"schema": 1, "kind": "intent-provenance", "id": "intent-gh", "provider": "codex", "goal": "", "ts": "2026-01-01T00:00:00Z", "source": {"schema": 1, "kind": "oms-intent-source", "source": {"kind": "github-issue", "reference": format!("https://github.com/{owner}/example-repo/issues/9")}}});
        repo.write(
            ".oms/intents/intent-gh.json",
            value::pretty(&provenance).unwrap(),
        );
        let planner = repo.planner(ANSWER);
        let (code, _, err) = intake("intent", &repo, &planner, &["adopt", "--id", "intent-gh"]);
        assert_eq!(code, 0, "{err}");
        let record = json_file(repo.0.join(".oms/intents/intent-gh.adopt.json"));
        assert_eq!(
            record["project_sha256"],
            value::hash(fs::read(repo.0.join("PROJECT.md")).unwrap())
        );
        let (code, out, err) = intake("plan-from-spec", &repo, &planner, &[]);
        assert_eq!(code, 0, "{err}");
        let (path, _) = printed(&out);
        assert_eq!(
            json_file(&path).get("issue").and_then(|v| v.as_str()),
            carried,
            "{owner}"
        );
        assert_eq!(out.contains(pending), carried.is_none(), "{out}");
    }
}

#[test]
fn autopilot_blocks_an_issue_less_proposal_until_restamped() {
    if scrubbed_child("autopilot_blocks_an_issue_less_proposal_until_restamped") {
        return;
    }
    let repo = Repo::github(ORIGIN, Some(SPEC));
    let planner = repo.planner(ANSWER);
    let run_file = repo.0.join(".oms/plan/autopilot-run.json");
    let (code, out, err) = intake("autopilot", &repo, &planner, &["propose"]);
    assert_eq!(code, 4, "{err}");
    let (path, sha) = printed(&out);
    assert!(!out.contains("plan-from-spec: pending:"), "{out}");
    assert!(out.contains(&format!("autopilot: pending: GitHub project mode requires an issue; run: oms autopilot propose --restamp {path} --expected-proposal-sha256 {sha} --issue REF")), "{out}");
    let row = json_file(&run_file);
    assert_eq!(
        (row["stage"].as_str(), row["blocked"].as_str()),
        (Some("proposal-review"), Some("github-issue"))
    );
    let before = fs::read(&run_file).unwrap();
    let run = [
        "run",
        "--proposal",
        path.as_str(),
        "--expected-proposal-sha256",
        sha.as_str(),
    ];
    let (code, _, err) = intake("autopilot", &repo, &planner, &run);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("oms autopilot propose --restamp"), "{err}");
    assert_eq!(fs::read(&run_file).unwrap(), before);
    let restamp = |sha: &str| {
        let args = [
            "propose",
            "--restamp",
            path.as_str(),
            "--expected-proposal-sha256",
            sha,
            "--issue",
            "7",
        ];
        intake("autopilot", &repo, &planner, &args)
    };
    assert_eq!(restamp(&"0".repeat(64)).0, 2);
    assert_eq!(fs::read(&run_file).unwrap(), before);
    let (code, out, err) = restamp(&sha);
    assert_eq!(code, 4, "{err}");
    let after = json_file(&run_file);
    assert!(after.get("blocked").is_none());
    assert_eq!(after["issue"], ISSUE);
    assert_eq!(after["stage"], "proposal-review");
    assert_ne!(after["proposal"]["sha256"], sha.as_str());
    assert_eq!(
        json_file(after["proposal"]["path"].as_str().unwrap())["tasks"][0]["issue"],
        ISSUE
    );
    assert!(
        out.contains(&format!(
            "oms autopilot run --proposal {} --expected-proposal-sha256 {}",
            after["proposal"]["path"].as_str().unwrap(),
            after["proposal"]["sha256"].as_str().unwrap()
        )),
        "{out}"
    );
    assert_eq!(
        receipt::contract_digest(&row),
        receipt::contract_digest(&after)
    );
    assert_eq!(
        planner.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "restamp is provider-free"
    );

    // An explicit --issue survives a failed proposal in the receipt.
    let repo = Repo::github(ORIGIN, Some(SPEC));
    let refused = ANSWER.replace(r#""depends": []"#, r#""depends": [], "issue": "7""#);
    let (code, _, err) = intake(
        "autopilot",
        &repo,
        &repo.planner(&refused),
        &["propose", "--issue", "7"],
    );
    assert_eq!(code, 2, "{err}");
    let row = json_file(repo.0.join(".oms/plan/autopilot-run.json"));
    assert_eq!(
        (row["stage"].as_str(), row["issue"].as_str()),
        (Some("proposing"), Some(ISSUE))
    );
    let (code, _, err) = intake(
        "autopilot",
        &repo,
        &repo.planner(ANSWER),
        &["propose", "--issue", "8"],
    );
    assert_eq!(code, 2);
    assert!(err.contains("already binds a different issue"), "{err}");
}
