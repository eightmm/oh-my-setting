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

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(value::random("oms-autonomy-").unwrap());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let repo = Self(path);
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
        let path = self.0.join(path);
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
            .write_all(body.as_ref())
            .unwrap();
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
        fs::remove_dir_all(&self.0).unwrap();
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

#[test]
fn selectors_keep_legacy_manifest_parity() {
    let repo = Repo::new();
    repo.suite();
    repo.write("tests/operator/runner.py", "SECTIONS = (\n    (\"native-pty\", 1, \"native_pty.py\"),\n    (\"dispatch-exit\", 2, None),\n    (\"shared-cache\", 3, \"shared-cache.sh\"),\n)\n");
    repo.write(
        "tests/operator/native_pty.py",
        "# BEGIN named case (details)\n# END named case\n",
    );
    repo.write("tests/operator/shared-cache.sh", "# BEGIN shell case\n");
    for args in [
        "--section native-pty,dispatch-exit",
        "--section native-pty --case 'named case'",
        "--section dispatch-exit --case 'inline case'",
        "--section shell-only",
    ] {
        repo.select(args).unwrap();
    }
    for args in [
        "--section missing",
        "--section native-pty --case missing",
        "--section shared-cache --case 'shell case'",
    ] {
        assert!(repo.select(args).is_err(), "{args}");
    }
}

#[test]
fn selectors_without_runner_accept_shell_sections_and_cases() {
    let repo = Repo::new();
    repo.suite();
    repo.write(
        "tests/operator/file_section.sh",
        "# BEGIN file case. Details\n# END file case\n",
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
fn selectors_refuse_unsafe_or_non_utf8_registry_and_case_files() {
    use std::os::unix::fs::symlink;
    let repo = Repo::new();
    repo.suite();
    repo.write("tests/operator/runner.py", [0xff]);
    assert!(repo.select("--section dispatch-exit").is_err());
    fs::remove_file(repo.0.join("tests/operator/runner.py")).unwrap();
    symlink(
        "../operator-tools-smoke.sh",
        repo.0.join("tests/operator/runner.py"),
    )
    .unwrap();
    assert!(repo.select("--section dispatch-exit").is_err());
    fs::remove_file(repo.0.join("tests/operator/runner.py")).unwrap();
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
}
