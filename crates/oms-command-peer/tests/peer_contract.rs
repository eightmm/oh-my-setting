#![forbid(unsafe_code)]
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn clean_env_child(command: &mut Command) -> &mut Command {
    command.env_clear();
    for name in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
}
const CHILD_ENV: &str = "CONTRACT_CHILD";
fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        body();
        return;
    }
    let home = Repo::new();
    // Create private fixtures without inheriting a shared-host umask.
    let mut command = Command::new("bash");
    let output = clean_env_child(&mut command)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "umask 077; exec \"$@\"",
            "--",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("HOME", &home.0)
        .env(CHILD_ENV, name)
        .output()
        .unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{out}");
    assert!(out.contains("1 passed;"), "child ran no test: {out}");
}
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oms-contract-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repo = Self(path.canonicalize().unwrap());
        repo.git(&["init", "-q"]);
        repo
    }
    fn git(&self, args: &[&str]) {
        let mut command = Command::new("git");
        let output = clean_env_child(&mut command)
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn write(&self, name: &str, body: impl AsRef<[u8]>) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
static STOP: AtomicBool = AtomicBool::new(false);
use oms_command_peer::process::{Completed, Executor, Invocation, Native};
use oms_command_peer::{Output, V, owns, run_with_source};
use std::{os::unix::fs::PermissionsExt, sync::Mutex, time::Duration};

const PROVIDER_BINS: &[&str] = &[
    "codex",
    "claude",
    "agy",
    "cursor-agent",
    "grok",
    "gemini",
    "qwen",
    "opencode2",
    "dsh",
    "vibe",
    "pi",
    "copilot",
    "droid",
    "aider",
    "muse",
    "kimi",
    "kiro-cli",
    "cn",
    "goose",
    "cline",
    "kilo",
    "openhands",
    "auggie",
];
fn executable_fixture(repo: &Repo, name: &str) {
    repo.write(name, "#!/bin/sh\nprintf 'fixture answer\\n'\n");
    fs::set_permissions(repo.0.join(name), fs::Permissions::from_mode(0o755)).unwrap();
}
fn provider_isolated(name: &str, layout: &str, body: impl FnOnce()) {
    provider_isolated_with_env(name, layout, &[], body);
}
fn provider_isolated_with_env(name: &str, layout: &str, env: &[(&str, &str)], body: impl FnOnce()) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        if std::env::var("PROVIDER_LAYOUT").as_deref() == Ok(layout) {
            body();
        }
        return;
    }
    let fixture = Repo::new();
    for bin in PROVIDER_BINS {
        executable_fixture(&fixture, &format!("bin/{bin}"));
    }
    for bin in ["phantom", "disabled"] {
        executable_fixture(&fixture, &format!("bin/{bin}"));
    }
    for dir in [
        "adapters",
        "xdg/oh-my-setting/provider-adapters",
        ".config/oh-my-setting/provider-adapters",
        "bin",
    ] {
        for provider in ["fixture", "blocked"] {
            executable_fixture(&fixture, &format!("{dir}/oms-agent-adapter-{provider}"));
        }
    }
    for name in [
        "a".repeat(64),
        "a".repeat(65),
        ".bad".into(),
        "-bad".into(),
        "_bad".into(),
        "bad..id".into(),
    ] {
        executable_fixture(&fixture, &format!("bin/oms-agent-adapter-{name}"));
    }
    fixture.write("bin/oms-agent-adapter-disabled", "inert\n");
    fs::create_dir(fixture.0.join("tmp")).unwrap();
    let mut paths = vec![fixture.0.join("bin")];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = if layout == "private" {
        let mut command = Command::new("bash");
        command
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "umask 077; exec \"$@\"",
                "--",
            ])
            .arg(std::env::current_exe().unwrap());
        command
    } else {
        Command::new(std::env::current_exe().unwrap())
    };
    let output = clean_env_child(&mut command)
        .args(["--exact", name, "--nocapture"])
        .env(CHILD_ENV, name)
        .env("PROVIDER_LAYOUT", layout)
        .env("HOME", &fixture.0)
        .env("TMPDIR", fixture.0.join("tmp"))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env(
            "OMS_PROVIDER_ADAPTER_DIR",
            if layout == "override" {
                fixture.0.join("adapters")
            } else {
                PathBuf::new()
            },
        )
        .env(
            "XDG_CONFIG_HOME",
            if layout == "home" {
                PathBuf::new()
            } else {
                fixture.0.join("xdg")
            },
        )
        .env("OMS_PROVIDER_WRITE_ADAPTERS", "other, fixture ,")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
}
struct ObservedProvider {
    request: Invocation,
    prompt: Vec<u8>,
    scratch: Option<PathBuf>,
}
#[derive(Default)]
struct ProviderProbe {
    calls: Mutex<Vec<ObservedProvider>>,
    outcome: i32,
    mutate: bool,
}
fn option_value<'a>(argv: &'a [OsString], name: &str) -> Option<&'a OsString> {
    argv.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| &pair[1])
}
impl Executor for ProviderProbe {
    fn execute(
        &self,
        request: Invocation,
        stop: &AtomicBool,
    ) -> oms_command_peer::Result<Completed> {
        if request.argv[0] == "git" {
            return Native.execute(request, stop);
        }
        let file = ["--prompt-file", "--message-file", "-f"]
            .into_iter()
            .find_map(|flag| option_value(&request.argv, flag))
            .map(PathBuf::from);
        let file = file.or_else(|| {
            option_value(&request.argv, "--print")?
                .to_str()?
                .strip_prefix("Read ")?
                .strip_suffix(" in full and follow it as your complete instructions.")
                .map(PathBuf::from)
        });
        let scratch = file.as_ref().map(|path| path.parent().unwrap().to_owned());
        if let Some(file) = &file {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(file.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        if let Some(empty) = option_value(&request.argv, "--env-file") {
            assert!(fs::read(empty).unwrap().is_empty());
            assert_eq!(
                fs::metadata(empty).unwrap().permissions().mode() & 0o777,
                0o600
            );
            for flag in ["--input-history-file", "--chat-history-file"] {
                assert_eq!(
                    Path::new(option_value(&request.argv, flag).unwrap()).parent(),
                    scratch.as_deref()
                );
            }
        }
        let prompt = if let Some(file) = file {
            fs::read(file).unwrap()
        } else if let Some(bytes) = &request.input {
            bytes.clone()
        } else {
            ["--print", "--prompt", "-p", "-t"]
                .into_iter()
                .find_map(|flag| option_value(&request.argv, flag))
                .unwrap_or_else(|| request.argv.last().unwrap())
                .as_encoded_bytes()
                .to_vec()
        };
        if self.mutate {
            fs::write(request.cwd.join("tracked.txt"), "provider change\n").unwrap();
        }
        self.calls.lock().unwrap().push(ObservedProvider {
            request,
            prompt,
            scratch,
        });
        if self.outcome == -1 {
            return Err(oms_command_peer::Error::new(
                125,
                "fixture execution failed",
            ));
        }
        Ok(Completed {
            code: self.outcome,
            stdout: b"fixture answer\n".to_vec(),
            stderr: Vec::new(),
            complete: true,
            cleanup_complete: Some(true),
            owned_cleanup: None,
            elapsed: Duration::ZERO,
        })
    }
}
fn provider_repo() -> Repo {
    let repo = Repo::new();
    repo.write("tracked.txt", "baseline\n");
    repo.git(&["add", "tracked.txt"]);
    repo.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    repo
}
fn provider_call(
    repo: &Repo,
    provider: &str,
    model: &str,
    effort: &str,
    question: &[u8],
    probe: &ProviderProbe,
) -> Output {
    repo.write("question.md", question);
    let values = args(&[
        "--providers",
        provider,
        "--model",
        model,
        "--reasoning-effort",
        effort,
        "--prompt-file",
        "question.md",
        "--no-task",
        "--no-memory",
        "--no-verify",
        "--no-ml-context",
        "--no-graph-context",
        "--allow-no-change",
    ]);
    oms_command_peer::run_with_executor(
        "peer-ask",
        &values,
        &repo.0,
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
        &STOP,
        probe,
    )
}

fn native_provider_answer(binary: &std::ffi::OsStr, answer: &[u8]) -> Vec<u8> {
    use serde_json::{json, to_vec, to_vec_pretty};

    let answer = std::str::from_utf8(answer).unwrap();
    if binary == "codex" {
        [
            json!({"type":"thread.started","thread_id":"fixture"}),
            json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"type":"agent_message","text":answer}}),
            json!({"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}),
        ]
        .into_iter()
        .flat_map(|row| [to_vec(&row).unwrap(), vec![b'\n']].concat())
        .collect()
    } else if binary == "claude" {
        [
            json!({"type":"system","subtype":"init"}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":answer}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"result":answer}),
        ]
        .into_iter()
        .flat_map(|row| [to_vec(&row).unwrap(), vec![b'\n']].concat())
        .collect()
    } else {
        assert_eq!(binary, "agy");
        to_vec_pretty(
            &json!({"conversation_id":"native-fixture","status":"SUCCESS","response":answer}),
        )
        .unwrap()
    }
}

#[test]
fn typed_agent_output_requires_terminal_provider_evidence() {
    provider_isolated(
        "typed_agent_output_requires_terminal_provider_evidence",
        "private",
        || {
            use oms_command_telemetry::store::usage::{self, Phase};

            struct NativeReport {
                probe: ProviderProbe,
                stdout: Vec<u8>,
                code: i32,
                complete: bool,
            }
            impl Executor for NativeReport {
                fn execute(
                    &self,
                    req: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    let git = req.argv[0] == "git";
                    let mut result = self.probe.execute(req, stop)?;
                    if !git {
                        result.stdout.clone_from(&self.stdout);
                        result.code = self.code;
                        result.complete = self.complete;
                    }
                    Ok(result)
                }
            }
            let answer = b"Native fixture response.";
            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            for (provider, binary, failed) in [
                (
                    "codex",
                    "codex",
                    serde_json::json!({"type":"turn.failed","error":{"message":"fixture failure"}}),
                ),
                (
                    "claude",
                    "claude",
                    serde_json::json!({"type":"result","subtype":"error_during_execution",
                        "is_error":true,"result":"Native fixture response."}),
                ),
                (
                    "antigravity",
                    "agy",
                    serde_json::json!({"conversation_id":"native-fixture",
                        "status":"ERROR","response":"Native fixture response."}),
                ),
            ] {
                let valid = native_provider_answer(binary.as_ref(), answer);
                let truncated = if provider == "antigravity" {
                    &valid[..valid.len() - 1]
                } else {
                    let terminal_start = valid[..valid.len() - 1]
                        .iter()
                        .rposition(|b| *b == b'\n')
                        .unwrap();
                    &valid[..=terminal_start]
                };
                let corrupt = [valid.as_slice(), b"\n{"].concat();
                let failed = serde_json::to_vec(&failed).unwrap();
                let failed = if provider == "antigravity" {
                    failed
                } else {
                    [truncated, &failed, b"\n"].concat()
                };
                for (raw, code, complete, expected) in [
                    (valid.as_slice(), 0, true, 0),
                    (answer.as_slice(), 0, true, 2),
                    (truncated, 0, true, 2),
                    (&failed, 0, true, 2),
                    (&corrupt, 0, true, 2),
                    (valid.as_slice(), 1, true, 2),
                    (valid.as_slice(), 0, false, 2),
                    (valid.as_slice(), 130, false, 130),
                ] {
                    let repo = provider_repo();
                    let report = NativeReport {
                        probe: ProviderProbe::default(),
                        stdout: raw.to_vec(),
                        code,
                        complete,
                    };
                    let options = args(&[
                        "--to",
                        provider,
                        "--model",
                        "fixture-model",
                        "--prompt",
                        "fixture question",
                        "--output-json",
                    ]);
                    let result = oms_command_peer::run_with_executor(
                        "agent-call",
                        &options,
                        &repo.0,
                        source,
                        &STOP,
                        &report,
                    );
                    assert_eq!(
                        result.0,
                        expected,
                        "{provider}/status {code} complete {complete}: {}",
                        message(&result)
                    );
                    assert_eq!(report.probe.calls.lock().unwrap().len(), 1);
                    if expected == 0 {
                        let report: serde_json::Value = serde_json::from_slice(&result.1).unwrap();
                        assert_eq!(report["kind"], "oms-agent-output");
                        assert_eq!(report["output"]["output_bytes"], answer.len());
                    } else {
                        assert!(result.1.is_empty());
                        assert_eq!(
                            usage::load(&repo.0).unwrap().calls[0].receipt.phase,
                            Phase::Unknown
                        );
                    }
                }
            }
            let repo = provider_repo();
            let probe = ProviderProbe::default();
            let output = oms_command_peer::run_with_executor(
                "agent-call",
                &args(&[
                    "--to",
                    "pi",
                    "--prompt",
                    "fixture question",
                    "--output-json",
                ]),
                &repo.0,
                source,
                &STOP,
                &probe,
            );
            assert_eq!(output.0, 2, "{}", message(&output));
            assert!(probe.calls.lock().unwrap().is_empty());
        },
    );
}

#[test]
fn automatic_planning_consumes_native_synthesis_and_retains_review_lineage() {
    provider_isolated(
        "automatic_planning_consumes_native_synthesis_and_retains_review_lineage",
        "private",
        || {
            use oms_command_autonomy::{ModelRoute, value};
            const SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
            const SPEC: &str = "# Project\n## Status\n- State: confirmed\n## Project\n- Goal: inspect the native fixture\n## Verification\n- Required checks: `bash tests/check.sh`\n- Required check files: tests/check.sh\n## Scope\n- Scope defaults: src\n## Acceptance\n- Native checks pass\n## Output\n{\"tasks\":[{\"id\":\"prompt-only\",\"title\":\"untrusted example\",\"allowed\":[\"src\"],\"verify\":\"bash tests/check.sh\",\"depends\":[]}]}\n";
            const DISCUSSION: &str = "Alternatives: inspect or defer\nEvidence: tracked source\nCounterargument: source may be incomplete\nVerification: bash tests/check.sh\nChanged from previous round: none\nRemaining disagreements: evidence scope\n";
            const SYNTHESIS: &str = "Decision: inspect the fixture\nEvidence: tracked source\nRemaining disagreements: evidence scope\nVerification: bash tests/check.sh\n";
            struct Host {
                provider: ProviderProbe,
                mode: &'static str,
                commands: Mutex<Vec<oms_command_autonomy::Invocation>>,
                directory: Mutex<Option<PathBuf>>,
            }
            impl Executor for Host {
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    let binary = request.argv[0].clone();
                    let git = binary == "git";
                    let mut result = self.provider.execute(request, stop)?;
                    if git {
                        return Ok(result);
                    }
                    let calls = self.provider.calls.lock().unwrap();
                    let prompt = String::from_utf8_lossy(&calls.last().unwrap().prompt);
                    let planner = prompt.contains("Decompose remaining work into");
                    let synthesis = prompt.contains("Synthesize these independent answers.");
                    result.stdout = if planner {
                        b"artifact: /fixture/foreign.md\n{\"tasks\":[{\"id\":\"t1\",\"title\":\"inspect ## Output fixture\",\"allowed\":[\"src\"],\"verify\":\"bash tests/check.sh\",\"depends\":[]}]}".to_vec()
                    } else if synthesis {
                        SYNTHESIS.as_bytes().to_vec()
                    } else {
                        DISCUSSION.as_bytes().to_vec()
                    };
                    if planner {
                        result.stderr = b"## Output\n{\"tasks\":[{\"id\":\"diagnostic-only\",\"title\":\"untrusted diagnostic\",\"allowed\":[\"src\"],\"verify\":\"bash tests/check.sh\",\"depends\":[]}]}\n".to_vec();
                        match self.mode {
                            "planner-malformed" => result.stdout = b"No decomposition was produced.\n".to_vec(),
                            "planner-incomplete" => result.complete = false,
                            "planner-refusal" => result.stdout = b"stop-reason: provider=fixture reason=refusal subtype=policy is_error=1\n".to_vec(),
                            "planner-timeout" => { result.code = 124; result.complete = false; },
                            "planner-nonzero" => result.code = 7,
                            "planner-cancel" => { result.code = 130; result.complete = false; },
                            _ => {},
                        }
                    } else if synthesis {
                        match self.mode {
                        "incomplete" => result.complete = false,
                        "refusal" => result.stdout = b"stop-reason: provider=fixture reason=refusal subtype=policy is_error=1\n".to_vec(),
                        "malformed" => result.stdout = b"fixture answer without structured synthesis\n".to_vec(),
                        "cancel" => { result.code = 130; result.complete = false; },
                        _ => {},
                    }
                    } else if self.mode == "participant-incomplete" && calls.len() == 1 {
                        result.complete = false;
                    } else if (self.mode == "participant-malformed-initial" && calls.len() == 1)
                        || (self.mode == "participant-malformed-rebuttal" && calls.len() == 4)
                    {
                        result.stdout =
                            b"A fluent answer missing required discussion sections.".to_vec();
                    }
                    if self.mode != "legacy"
                        && !(planner && self.mode == "planner-no-native-terminal")
                        && !(synthesis && self.mode == "synthesis-no-native-terminal")
                    {
                        result.stdout = native_provider_answer(&binary, &result.stdout);
                    }
                    Ok(result)
                }
            }
            impl oms_command_plan::Services for Host {}
            impl oms_command_autonomy::Services for Host {
                fn resolve_route(
                    &self,
                    _: &Path,
                    assignment: &serde_json::Value,
                    role: &str,
                    _: &AtomicBool,
                ) -> oms_command_autonomy::Result<ModelRoute> {
                    let route = oms_command_peer::providers::resolve_route(
                        Path::new(SOURCE),
                        value::text(assignment, "provider"),
                        value::text(assignment, "model"),
                        value::text(assignment, "reasoning_effort"),
                        role,
                    )
                    .map_err(|e| oms_command_autonomy::Error::new(e.code, e.message))?;
                    Ok(ModelRoute {
                        provider: route.provider,
                        model_class: "standard".into(),
                        primary: route.model,
                        fallback: value::text(assignment, "fallback_model").into(),
                        effort: if route.effort.is_empty() {
                            "auto".into()
                        } else {
                            route.effort
                        },
                        fallback_effort: "auto".into(),
                    })
                }
                fn invoke(
                    &self,
                    command: &oms_command_autonomy::Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_autonomy::Result<oms_command_autonomy::CommandResult>
                {
                    assert!(["peer-ask", "agent-call"].contains(&command.tool.as_str()));
                    self.commands.lock().unwrap().push(command.clone());
                    if command.tool == "peer-ask" {
                        // The production caller formerly forgot this flag, so a successful
                        // real native council could never satisfy its own consumer.
                        assert!(option_value(&command.args, "--synthesize").is_some());
                        assert!(command.args.iter().any(|arg| arg == "--synthesis-json"));
                        *self.directory.lock().unwrap() = Some(PathBuf::from(
                            option_value(&command.args, "--artifact-dir").unwrap(),
                        ));
                    }
                    let mut output = oms_command_peer::run_with_executor(
                        &command.tool,
                        &command.args,
                        &command.cwd,
                        Path::new(SOURCE),
                        stop,
                        self,
                    );
                    if command.tool == "peer-ask" && output.0 == 0 {
                        if self.mode == "missing-report" {
                            output.1 = b"synthesis: unbound.md\n".to_vec();
                        } else if self.mode == "foreign-report" {
                            let mut report = value::parse(&output.1).unwrap();
                            report["prompt_sha256"] = "0".repeat(64).into();
                            output.1 = value::pretty(&report).unwrap();
                        }
                    }
                    if command.tool == "agent-call" && self.mode == "mutate-during-plan" {
                        fs::write(
                            self.directory
                                .lock()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .join("question.md"),
                            b"changed question",
                        )
                        .unwrap();
                    }
                    Ok(output)
                }
            }
            let preflight_repo = provider_repo();
            let preflight_probe = ProviderProbe::default();
            for synthesize in [false, true] {
                let mut options = args(&[
                    "--providers",
                    "codex:model=first,codex:model=second",
                    "--debate",
                    "1",
                    "--deliberation",
                    "--require-complete",
                    "--synthesis-json",
                    "--prompt",
                    "fixture question",
                ]);
                if synthesize {
                    options.extend(args(&["--synthesize", "codex:model=first"]));
                }
                let output = oms_command_peer::run_with_executor(
                    "peer-ask",
                    &options,
                    &preflight_repo.0,
                    Path::new(SOURCE),
                    &STOP,
                    &preflight_probe,
                );
                assert_eq!(output.0, 2, "{}", message(&output));
                assert!(preflight_probe.calls.lock().unwrap().is_empty());
            }
            let typed_council = |target: &str, host: &dyn Executor| {
                oms_command_peer::run_with_executor(
                    "peer-ask",
                    &args(&[
                        "--providers",
                        "codex:model=first,claude:model=second,antigravity:model=third",
                        "--reasoning-effort",
                        "high",
                        "--debate",
                        "2",
                        "--deliberation",
                        "--require-complete",
                        "--synthesis-json",
                        "--synthesize",
                        target,
                        "--prompt",
                        "fixture question",
                    ]),
                    &preflight_repo.0,
                    Path::new(SOURCE),
                    &STOP,
                    host,
                )
            };
            for (target, reason) in [
                (
                    "codex:model=provider-default",
                    "exact synthesis target model",
                ),
                (
                    "antigravity:model=gemini-pro-orchestrator-argon",
                    "has fixed effort",
                ),
                (
                    "codex:model=exact-synthesis:account=absent",
                    "account is unavailable",
                ),
                (
                    "pi:model=exact-synthesis",
                    "cannot prove native output completion",
                ),
            ] {
                let output = typed_council(target, &preflight_probe);
                assert_eq!(output.0, 2, "{target}: {}", message(&output));
                assert!(
                    message(&output).contains(reason),
                    "{target}: {}",
                    message(&output)
                );
                assert!(preflight_probe.calls.lock().unwrap().is_empty());
            }
            for (mode, target, effort, count, expected) in [
                ("ok", "codex:model=exact-synthesis", "high", 10, 0),
                ("ok", "codex:effort=low:model=exact-synthesis", "low", 10, 0),
                (
                    "participant-malformed-rebuttal",
                    "codex:model=exact-synthesis",
                    "high",
                    6,
                    2,
                ),
            ] {
                let host = Host {
                    provider: ProviderProbe::default(),
                    mode,
                    commands: Mutex::new(Vec::new()),
                    directory: Mutex::new(None),
                };
                let output = typed_council(target, &host);
                assert_eq!(output.0, expected, "{mode}/{target}: {}", message(&output));
                let calls = host.provider.calls.lock().unwrap();
                assert_eq!(calls.len(), count);
                if expected == 0 {
                    let argv = &calls.last().unwrap().request.argv;
                    assert_eq!(option_value(argv, "--model").unwrap(), "exact-synthesis");
                    assert_eq!(
                        option_value(argv, "-c").unwrap(),
                        &OsString::from(format!("model_reasoning_effort=\"{effort}\""))
                    );
                }
            }
            let legacy = Host {
                provider: ProviderProbe::default(),
                mode: "legacy",
                commands: Mutex::new(Vec::new()),
                directory: Mutex::new(None),
            };
            let output = oms_command_peer::run_with_executor(
                "agent-call",
                &args(&[
                    "--to",
                    "codex",
                    "--model",
                    "gpt-5.6-sol",
                    "--prompt",
                    "Decompose remaining work into one fixture task",
                ]),
                &preflight_repo.0,
                Path::new(SOURCE),
                &STOP,
                &legacy,
            );
            assert_eq!(output.0, 0, "{}", message(&output));
            let text = String::from_utf8(output.1).unwrap();
            assert!(text.contains("artifact: /fixture/foreign.md"));
            assert!(text.lines().last().unwrap().starts_with(&format!(
                "artifact: {}/.oms/artifacts/call/",
                preflight_repo.0.display()
            )));
            for (mode, planner, model, expected) in [
                ("ok", "codex", "gpt-5.6-sol", 0),
                ("ok", "codex", "", 0),
                ("ok", "claude", "claude-sonnet-5-5", 0),
                ("ok", "antigravity", "gemini-pro-orchestrator-argon", 0),
                ("participant-incomplete", "codex", "gpt-5.6-sol", 2),
                ("participant-malformed-initial", "codex", "gpt-5.6-sol", 2),
                ("participant-malformed-rebuttal", "codex", "gpt-5.6-sol", 2),
                ("synthesis-no-native-terminal", "codex", "gpt-5.6-sol", 2),
                ("incomplete", "codex", "gpt-5.6-sol", 2),
                ("refusal", "codex", "gpt-5.6-sol", 2),
                ("malformed", "codex", "gpt-5.6-sol", 2),
                ("cancel", "codex", "gpt-5.6-sol", 130),
                ("missing-report", "codex", "gpt-5.6-sol", 2),
                ("foreign-report", "codex", "gpt-5.6-sol", 2),
                ("mutate-during-plan", "codex", "gpt-5.6-sol", 2),
                ("planner-malformed", "codex", "gpt-5.6-sol", 3),
                ("planner-incomplete", "codex", "gpt-5.6-sol", 2),
                ("planner-refusal", "codex", "gpt-5.6-sol", 2),
                ("planner-timeout", "codex", "gpt-5.6-sol", 2),
                ("planner-nonzero", "codex", "gpt-5.6-sol", 2),
                ("planner-no-native-terminal", "codex", "gpt-5.6-sol", 2),
                ("planner-cancel", "codex", "gpt-5.6-sol", 130),
            ] {
                let repo = provider_repo();
                repo.git(&["branch", "-M", "main"]);
                repo.write(".git/info/exclude", ".oms/\n");
                repo.write("PROJECT.md", SPEC);
                repo.write("tests/check.sh", "exit 1\n");
                repo.write("src/lib.rs", "pub fn fixture() {}\n");
                repo.git(&["add", "PROJECT.md", "tests", "src"]);
                repo.git(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "contract",
                ]);
                let host = Host {
                    provider: ProviderProbe::default(),
                    mode,
                    commands: Mutex::new(Vec::new()),
                    directory: Mutex::new(None),
                };
                let mut options = args(&[
                    "--collaboration",
                    "auto",
                    "--to",
                    planner,
                    "--worker-provider",
                    "antigravity",
                    "--reasoning-effort",
                    "auto",
                ]);
                if !model.is_empty() {
                    options.extend(args(&["--model", model]));
                }
                let expected_route = oms_command_peer::providers::resolve_route(
                    Path::new(SOURCE),
                    planner,
                    model,
                    "auto",
                    "advisor",
                )
                .unwrap();
                let output = oms_command_autonomy::run_with_services(
                    "plan-from-spec",
                    &options,
                    &repo.0,
                    &STOP,
                    &host,
                );
                assert_eq!(output.0, expected, "{mode}/{planner}: {}", message(&output));
                let commands = host.commands.lock().unwrap();
                let planned = commands.iter().filter(|c| c.tool == "agent-call").count();
                assert_eq!(
                    planned,
                    usize::from(
                        ["ok", "mutate-during-plan"].contains(&mode)
                            || mode.starts_with("planner-")
                    ),
                    "{mode}"
                );
                let calls = host.provider.calls.lock().unwrap().len();
                match mode {
                    "participant-incomplete" | "participant-malformed-initial" => {
                        assert_eq!(calls, 3)
                    }
                    "participant-malformed-rebuttal" => assert_eq!(calls, 6),
                    _ => {}
                }
                let council = &commands[0];
                let target = option_value(&council.args, "--synthesize")
                    .unwrap()
                    .to_str()
                    .unwrap();
                assert!(
                    target.starts_with(&format!("{planner}:effort="))
                        && target.ends_with(&format!(":model={}", expected_route.model)),
                    "{target}"
                );
                assert!(
                    option_value(&council.args, "--reasoning-effort").is_none(),
                    "planner effort must not override all seats"
                );
                drop(commands);
                assert!(
                    !repo.0.join(".oms/plan/tasks.json").exists(),
                    "generation grants no Plan authority"
                );
                let proposals = fs::read_dir(repo.0.join(".oms/plan"))
                    .unwrap()
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with("proposal-"))
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>();
                assert_eq!(proposals.len(), usize::from(mode == "ok"), "{mode}");
                if mode != "ok" {
                    continue;
                }
                assert_eq!(host.provider.calls.lock().unwrap().len(), 8);
                let path = &proposals[0];
                let raw = fs::read(path).unwrap();
                let proposal = value::parse(&raw).unwrap();
                assert_eq!(proposal["tasks"][0]["title"], "inspect ## Output fixture");
                let digest = value::hash(&raw);
                let reference = &proposal["discussion"];
                let manifest =
                    value::parse(&fs::read(repo.0.join(value::text(reference, "path"))).unwrap())
                        .unwrap();
                assert_eq!(
                    manifest["report"]["participants"].as_array().unwrap().len(),
                    6
                );
                assert_eq!(
                    manifest["report"]["synthesis"]["requested_model"],
                    expected_route.model
                );
                assert!(
                    String::from_utf8_lossy(&output.1)
                        .contains(value::text(&manifest["report"], "run"))
                );
                let apply = || {
                    oms_command_autonomy::run_with_services(
                        "plan-from-spec",
                        &args(&[
                            "--apply",
                            path.to_str().unwrap(),
                            "--expected-proposal-sha256",
                            &digest,
                            "--collaboration",
                            "auto",
                            "--worker-provider",
                            "antigravity",
                        ]),
                        &repo.0,
                        &STOP,
                        &host,
                    )
                };
                let applied = apply();
                assert_eq!(applied.0, 0, "{}", message(&applied));
                assert_eq!(apply().0, 0, "idempotent replay");
                let plan =
                    value::parse(&fs::read(repo.0.join(".oms/plan/tasks.json")).unwrap()).unwrap();
                assert_eq!(&plan["tasks"]["t1"]["discussion"], reference);
                assert_eq!(
                    host.provider.calls.lock().unwrap().len(),
                    8,
                    "apply/replay must not rerun the council"
                );
                if planner != "codex" || model.is_empty() {
                    continue;
                }
                let direct = |file: &str, sha: &str| {
                    oms_command_plan::run_with_services(
                        &args(&[
                            "apply-proposal",
                            "--proposal",
                            file,
                            "--expected-proposal-sha256",
                            sha,
                            "--expected-plan-sha256",
                            "absent",
                            "--goal",
                            "inspect the native fixture",
                            "--accept",
                            "bash tests/check.sh",
                            "--accept-files",
                            "tests/check.sh",
                            "--allowed-envelope",
                            "src",
                        ]),
                        &repo.0,
                        &STOP,
                        &host,
                    )
                };
                assert_eq!(direct(path.to_str().unwrap(), &digest).0, 0);
                let duplicate = String::from_utf8(raw.clone()).unwrap().replacen(
                    "\"schema\": 1",
                    "\"schema\": 1, \"schema\": 1",
                    1,
                );
                repo.write(".oms/plan/duplicate.json", &duplicate);
                assert_eq!(
                    direct(".oms/plan/duplicate.json", &value::hash(&duplicate)).0,
                    2
                );
                let mut omitted = proposal.clone();
                omitted.as_object_mut().unwrap().remove("discussion");
                let omitted = value::pretty(&omitted).unwrap();
                repo.write(".oms/plan/omitted.json", &omitted);
                assert_eq!(
                    direct(".oms/plan/omitted.json", &value::hash(&omitted)).0,
                    2,
                    "replay cannot lose its retained discussion"
                );
                let plan_before = fs::read(repo.0.join(".oms/plan/tasks.json")).unwrap();
                let calls_before = host.provider.calls.lock().unwrap().len();
                let catalog = Path::new(value::text(reference, "path")).parent().unwrap();
                for (name, edit) in [
                    ("underquorum", ("providers", serde_json::json!(["codex"]))),
                    ("wrongrounds", ("rounds", serde_json::json!(2))),
                ] {
                    let mut pinned = manifest.clone();
                    pinned[edit.0] = edit.1;
                    let bytes = value::pretty(&pinned).unwrap();
                    let sha = value::hash(&bytes);
                    let retained = catalog.join(format!("{sha}.json"));
                    repo.write(retained.to_str().unwrap(), &bytes);
                    let mut rewired = proposal.clone();
                    rewired["discussion"] =
                        serde_json::json!({"path": retained.to_str().unwrap(), "sha256": sha});
                    let rewired = value::pretty(&rewired).unwrap();
                    repo.write(&format!(".oms/plan/{name}.json"), &rewired);
                    let result = direct(&format!(".oms/plan/{name}.json"), &value::hash(&rewired));
                    assert_eq!(result.0, 2, "{name}: {}", message(&result));
                    assert!(
                        message(&result).contains(
                            "discussion council differs from the automatic planning quorum"
                        ),
                        "{name}: {}",
                        message(&result)
                    );
                }
                assert_eq!(
                    fs::read(repo.0.join(".oms/plan/tasks.json")).unwrap(),
                    plan_before,
                    "rejected quorum must not mutate the plan"
                );
                assert_eq!(host.provider.calls.lock().unwrap().len(), calls_before);
                let route = serde_json::json!({"model": "fixture-model", "fallback_model": "", "reasoning_effort": "auto", "timeout": "1s"});
                let mut planner_route = route.clone();
                planner_route["model"] = expected_route.model.as_str().into();
                planner_route["reasoning_effort"] = if expected_route.effort.is_empty() {
                    "auto"
                } else {
                    expected_route.effort.as_str()
                }
                .into();
                let receipt = serde_json::json!({"schema":1,"kind":"autopilot-run","stage":"proposal-review","repo":repo.0,"spec_sha256":proposal["spec_sha256"],"providers":{"planner":planner,"worker":"antigravity","reviewer":"claude"},"routing":{"provider_timeout":"1s","planner":planner_route,"worker":route,"reviewer":route},"contract":{"allowed":"src","base":"main","base_sha":proposal["base_sha"],"remote":"origin","max_cycles":1,"initial_tasks":1,"replan_tasks":1,"auto_repair":false,"retry_known":false,"review_mode":"gate","draft_pr":false,"allow_verifier_change":false,"allow_floor_change":"","collaboration":"auto"},"proposal":{"path":path,"sha256":digest},"branch":format!("oms/autopilot-{}", &value::text(&proposal, "spec_sha256")[..12]),"owner_id":format!("owner_{}","a".repeat(32)),"updated":value::now()});
                oms_command_autonomy::receipt::validate(&receipt).unwrap();
                repo.write(
                    ".oms/plan/autopilot-run.json",
                    value::pretty(&receipt).unwrap(),
                );
                let resume = || {
                    oms_command_autonomy::run_with_services(
                        "autopilot",
                        &args(&["run"]),
                        &repo.0,
                        &STOP,
                        &host,
                    )
                };
                assert!(message(&resume()).contains("parent proposal review required"));
                let artifact = host
                    .directory
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .join(value::text(&manifest["report"]["synthesis"], "artifact"));
                let original = fs::read(&artifact).unwrap();
                fs::write(&artifact, b"changed synthesis").unwrap();
                assert_eq!(apply().0, 2);
                assert_eq!(direct(path.to_str().unwrap(), &digest).0, 2);
                assert!(message(&resume()).contains("native artifact bytes changed"));
                fs::write(&artifact, original).unwrap();
                assert_eq!(host.provider.calls.lock().unwrap().len(), 8);
            }
        },
    );
}

#[test]
fn council_seat_publication_stops_after_thread_append_failure() {
    provider_isolated_with_env(
        "council_seat_publication_stops_after_thread_append_failure",
        "override",
        &[("OMS_PANEL_DISPATCH", "1")],
        || {
            struct CouncilProbe<'a> {
                provider: ProviderProbe,
                selected: Mutex<Vec<(String, Option<String>)>>,
                repo: &'a Repo,
                close_thread: bool,
                incomplete: bool,
            }
            impl Executor for CouncilProbe<'_> {
                fn with_physical_contract(
                    &self,
                    attempt: &oms_command_peer::process::PhysicalAttempt<'_>,
                    deadline: std::time::Instant,
                    stop: &AtomicBool,
                    register: oms_command_peer::process::RegisterPhysical<'_>,
                ) -> oms_command_peer::Result<()> {
                    self.selected.lock().unwrap().push((
                        attempt.selected_model.unwrap_or_default().into(),
                        attempt.selected_effort.map(str::to_owned),
                    ));
                    self.provider
                        .with_physical_contract(attempt, deadline, stop, register)
                }
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    let git = request.argv[0] == "git";
                    let mut result = self.provider.execute(request, stop)?;
                    if !git && self.provider.calls.lock().unwrap().len() == 1 {
                        if self.close_thread {
                            success(run(
                                self.repo,
                                self.repo.0.as_path(),
                                "thread",
                                &["close", "--id", "fixture-council"],
                            ));
                        }
                        if self.incomplete {
                            result.code = 7;
                        }
                    }
                    Ok(result)
                }
            }
            for (close_thread, incomplete, expected_code, expected_calls) in [
                (true, false, 2, 1),
                (false, false, 0, 6),
                (false, true, 1, 2),
            ] {
                let repo = provider_repo();
                let probe = CouncilProbe {
                    provider: ProviderProbe::default(),
                    selected: Mutex::new(Vec::new()),
                    repo: &repo,
                    close_thread,
                    incomplete,
                };
                let mixed = !close_thread && !incomplete;
                let targets = if mixed {
                    "agy:effort=auto:model=gemini-pro-orchestrator-argon,codex:effort=medium:model=gpt-6.2-terra,claude:model=claude-sonnet-5-5"
                } else {
                    "agy,codex"
                };
                let output = oms_command_peer::run_with_executor(
                    "peer-ask",
                    &args(&[
                        "--providers",
                        targets,
                        "--reasoning-effort",
                        "high",
                        "--debate",
                        "1",
                        "--deliberation",
                        "--require-complete",
                        "--thread",
                        "fixture-council",
                        "--prompt",
                        "Inspect the fixture",
                        "--no-task",
                        "--no-memory",
                    ]),
                    &repo.0,
                    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                    &STOP,
                    &probe,
                );
                assert_eq!(output.0, expected_code, "{}", message(&output));
                assert_eq!(probe.provider.calls.lock().unwrap().len(), expected_calls);
                if mixed {
                    let selected = [
                        ("gemini-pro-orchestrator-argon", None),
                        ("gpt-6.2-terra", Some("medium")),
                        ("claude-sonnet-5-5", Some("high")),
                    ];
                    let physical = probe.selected.lock().unwrap();
                    // The peer checks the same route at registration and again at launch.
                    let expected = selected
                        .into_iter()
                        .cycle()
                        .take(expected_calls)
                        .flat_map(|seat| [seat, seat]);
                    assert!(
                        physical
                            .iter()
                            .map(|(model, effort)| (model.as_str(), effort.as_deref()))
                            .eq(expected),
                        "{physical:?}"
                    );
                    let receipts = oms_command_telemetry::store::usage::load(&repo.0).unwrap();
                    for (model, _) in selected {
                        assert_eq!(
                            receipts
                                .calls
                                .iter()
                                .filter(|call| {
                                    call.receipt.identity.selected_model.as_deref() == Some(model)
                                })
                                .count(),
                            2
                        );
                    }
                }
                let rows = oms_command_peer::storage::load(
                    &repo.0,
                    Path::new(".oms/artifacts/index.jsonl"),
                )
                .unwrap();
                assert_eq!(rows.len(), expected_calls);
                if close_thread {
                    assert!(output.1.is_empty());
                    assert_eq!(rows[0].get("provider").text(), Some("antigravity"));
                    let artifact = repo.0.join(rows[0].get("artifact").text().unwrap());
                    let body = fs::read_to_string(artifact).unwrap();
                    assert!(body.contains("## Output\n\nfixture answer"));
                    let turns = oms_command_peer::storage::load(
                        &repo.0,
                        Path::new(".oms/threads/fixture-council.jsonl"),
                    )
                    .unwrap();
                    let roles = turns.iter().map(|turn| turn.get("role").text().unwrap());
                    assert!(roles.eq(["question", "closed"]));
                    assert_eq!(
                        output.2,
                        b"error: council seat antigravity round 0: thread append failed; reconcile before retrying\n",
                    );
                } else {
                    let usable = if incomplete { "1/2" } else { "3/3" };
                    assert!(message(&output).contains(&format!("{usable} target(s) answered")));
                }
            }
        },
    );
}

#[test]
fn delegate_research_roles_expose_public_tools_without_file_write_authority() {
    assert_delegate_research_tools(
        "delegate_research_roles_expose_public_tools_without_file_write_authority",
        false,
    );
}

#[test]
fn delegate_research_roles_honor_panel_no_web() {
    assert_delegate_research_tools("delegate_research_roles_honor_panel_no_web", true);
}

fn assert_delegate_research_tools(test: &str, no_web: bool) {
    provider_isolated_with_env(
        test,
        "private",
        &[("OMS_PANEL_NO_WEB", if no_web { "1" } else { "0" })],
        || {
            let repo = provider_repo();
            for provider in ["codex", "claude"] {
                for role in ["", "repo-auditor", "researcher", "scholar"] {
                    let public = !no_web && matches!(role, "researcher" | "scholar");
                    let mut values = args(&[
                        "--to",
                        provider,
                        "--read-only",
                        "--model",
                        "fixture",
                        "--no-task",
                        "--no-memory",
                        "--no-ml-context",
                        "--no-graph-context",
                    ]);
                    if !role.is_empty() {
                        values.extend(args(&["--role", role]));
                    }
                    values.push("Inspect relevant sources and cite the evidence".into());
                    let probe = ProviderProbe::default();
                    let output = oms_command_peer::run_with_executor(
                        "peer-delegate",
                        &values,
                        &repo.0,
                        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                        &STOP,
                        &probe,
                    );
                    assert_eq!(output.0, 0, "{provider}/{role}: {}", message(&output));
                    let calls = probe.calls.lock().unwrap();
                    assert_eq!(calls.len(), 1);
                    let argv = &calls[0].request.argv;
                    assert_ne!(calls[0].request.cwd, repo.0);
                    assert!(!argv.iter().any(|a| a == "--dangerously-skip-permissions" || a == "--allowedTools"));
                    if provider == "codex" {
                        assert_eq!(option_value(argv, "--sandbox").unwrap(), "read-only");
                        assert!(argv.iter().any(|a| a == "features.apps=false"));
                        let search = if public {
                            "web_search=\"live\""
                        } else {
                            "web_search=\"disabled\""
                        };
                        assert!(
                            argv.iter().any(|a| a == search),
                            "{provider}/{role}: {argv:?}"
                        );
                    } else {
                        assert_eq!(option_value(argv, "--permission-mode").unwrap(), "plan");
                        let tools = option_value(argv, "--tools").unwrap().to_str().unwrap();
                        let expected = if public {
                            "Read,Grep,Glob,Bash,WebSearch,WebFetch"
                        } else {
                            "Read,Grep,Glob,Bash"
                        };
                        assert_eq!(tools, expected, "{provider}/{role}");
                        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
                    }
                }
            }
        },
    );
}

#[test]
fn provider_native_transports_match_shell_contract() {
    provider_isolated(
        "provider_native_transports_match_shell_contract",
        "override",
        || {
            let repo = provider_repo();
            // Frozen argv from scripts/lib/peer-common.sh; placeholders are invocation-private paths.
            let cases = [
                (
                    "codex",
                    "codex",
                    "exec|{model}|-c|model_reasoning_effort=\"high\"|--json|-c|web_search=\"disabled\"|-c|features.apps=false|--sandbox|read-only|--skip-git-repo-check|-",
                ),
                (
                    "claude",
                    "claude",
                    "--permission-mode|plan|{model}|--effort|high|{read}|--output-format|stream-json|--verbose|-p",
                ),
                (
                    "antigravity",
                    "agy",
                    "{model}|--add-dir|{cwd}|--effort|high|--sandbox|--print-timeout|{timeout}s|--output-format|json|--print|{prompt}",
                ),
                (
                    "cursor",
                    "cursor-agent",
                    "--print|--output-format|text|--workspace|{cwd}|--sandbox|enabled|{model}|--mode|ask",
                ),
                (
                    "grok",
                    "grok",
                    "--no-auto-update|--cwd|{cwd}|--output-format|plain|{model}|--effort|high|--permission-mode|dontAsk|--allow|Read|--allow|Grep|--allow|Bash(git *)|--deny|Edit|--deny|WebFetch|--deny|WebSearch|--sandbox|strict|--no-memory|--no-subagents|--disable-web-search|-p|{prompt}",
                ),
                (
                    "gemini",
                    "gemini",
                    "--output-format|text|--sandbox|{model}|--approval-mode|plan",
                ),
                (
                    "qwen",
                    "qwen",
                    "--output-format|text|--sandbox|{model}|--safe-mode|--approval-mode|plan",
                ),
                (
                    "opencode",
                    "opencode2",
                    "run|--format|default|{model}|--agent|plan|{prompt}",
                ),
                ("deepseek", "dsh", "--profile|headless|{prompt}"),
                (
                    "vibe",
                    "vibe",
                    "--trust|--max-turns|24|--output|text|--agent|plan|--enabled-tools|read_file|--enabled-tools|grep|--prompt|{prompt}",
                ),
                (
                    "pi",
                    "pi",
                    "--no-session|--no-approve|--no-extensions|--no-skills|--no-prompt-templates|--mode|json|{model}|--thinking|high|--tools|read,grep,find,ls|--print|{prompt}",
                ),
                (
                    "copilot",
                    "copilot",
                    "-s|--no-ask-user|--disable-builtin-mcps|--disallow-temp-dir|{model}|--effort|high|--available-tools|view,grep,glob|--deny-tool|write|--deny-tool|shell|--deny-tool|url|--deny-tool|memory|-p|{prompt}",
                ),
                (
                    "droid",
                    "droid",
                    "exec|--cwd|{cwd}|--output-format|text|--disable-builtin-skills|{model}|--reasoning-effort|high||-f|{file}",
                ),
                (
                    "aider",
                    "aider",
                    "--no-auto-commits|--no-dirty-commits|--no-gitignore|--no-suggest-shell-commands|--no-auto-lint|--no-auto-test|--no-check-update|--no-detect-urls|--no-pretty|--no-stream|--no-analytics|--no-restore-chat-history|--env-file|{env}|--input-history-file|{input}|--chat-history-file|{chat}|{model}|--reasoning-effort|high|--chat-mode|ask|--dry-run|--message-file|{file}",
                ),
                (
                    "muse",
                    "muse",
                    "exec|{model}|--reasoning-effort|high|--disable-approval|{prompt}",
                ),
                ("kimi", "kimi", "{model}|-p|{prompt}"),
                (
                    "kiro",
                    "kiro-cli",
                    "chat|--v3|--no-interactive|{model}|--effort|high|--trust-all-tools|{prompt}",
                ),
                ("continue", "cn", "-p|{prompt}|{model}|--readonly"),
                ("goose", "goose", "run|--no-session|{model}|-t|{prompt}"),
                (
                    "cline",
                    "cline",
                    "{model}|--thinking|high|--auto-approve|true|{prompt}",
                ),
                ("kilo", "kilo", "run|--auto|{model}|--variant|high|{prompt}"),
                ("openhands", "openhands", "--headless|-t|{prompt}"),
                (
                    "auggie",
                    "auggie",
                    "--print|{model}|--permission|edit:allow|--permission|write:allow|--permission|terminal:allow|{prompt}",
                ),
                (
                    "fixture",
                    "",
                    "run|--access|read|--workdir|{cwd}|--prompt-file|{file}|{model}|--effort|high",
                ),
            ];
            for (provider, bin, pattern) in cases {
                for exact in [false, true] {
                    let native_model = ["deepseek", "vibe", "openhands"].contains(&provider);
                    let model = if exact {
                        "fixture-model"
                    } else {
                        "provider-default"
                    };
                    let probe = ProviderProbe::default();
                    let output = provider_call(
                        &repo,
                        provider,
                        model,
                        "high",
                        "inspect λ\nnext line".as_bytes(),
                        &probe,
                    );
                    if exact && native_model {
                        assert_eq!(output.0, 1, "{provider}");
                        let artifact = String::from_utf8_lossy(&output.1)
                            .lines()
                            .find(|line| Path::new(line).is_file())
                            .map(PathBuf::from)
                            .expect("failed seat artifact");
                        assert!(
                            fs::read_to_string(artifact)
                                .unwrap()
                                .contains("no documented per-invocation model override"),
                            "{provider}: {}",
                            message(&output)
                        );
                        assert!(probe.calls.lock().unwrap().is_empty());
                        continue;
                    }
                    if [
                        "muse",
                        "kimi",
                        "kiro",
                        "goose",
                        "cline",
                        "kilo",
                        "openhands",
                        "auggie",
                    ]
                    .contains(&provider)
                    {
                        assert_ne!(output.0, 0, "{provider} read must fail");
                        assert!(probe.calls.lock().unwrap().is_empty());
                        continue;
                    }
                    success(output);
                    let calls = probe.calls.lock().unwrap();
                    assert_eq!(calls.len(), 1, "{provider}");
                    let call = &calls[0];
                    let request = &call.request;
                    assert_ne!(request.cwd, repo.0, "isolate {provider}");
                    assert!(!request.cwd.exists(), "clean disposable worktree");
                    let prompt = String::from_utf8(call.prompt.clone()).unwrap();
                    assert!(prompt.contains("inspect λ\nnext line"));
                    let mut pattern = pattern.replace(
                        "{model}",
                        if model == "provider-default" {
                            ""
                        } else if provider == "antigravity" {
                            "--model|fixture-model-high"
                        } else {
                            "--model|fixture-model"
                        },
                    );
                    if provider == "claude" {
                        let extra = format!(
                            "--strict-mcp-config|--setting-sources|{{empty}}|--tools|Read,Grep,Glob,Bash|--settings|{}",
                            option_value(&request.argv, "--settings")
                                .unwrap()
                                .to_str()
                                .unwrap()
                        );
                        pattern = pattern.replace("{read}", &extra);
                    }
                    let scratch = call.scratch.clone().unwrap_or_default();
                    let expected: Vec<OsString> = pattern
                        .split('|')
                        .filter(|s| !s.is_empty())
                        .map(|s| match s {
                            "{empty}" => OsString::new(),
                            "{cwd}" => request.cwd.as_os_str().into(),
                            "{prompt}" => prompt.clone().into(),
                            "{file}" => scratch.join("prompt.md").into_os_string(),
                            "{env}" => scratch.join("empty.env").into_os_string(),
                            "{input}" => scratch.join("input.history").into_os_string(),
                            "{chat}" => scratch.join("chat.history").into_os_string(),
                            "{timeout}s" => format!("{}s", request.timeout.as_secs()).into(),
                            _ => s.into(),
                        })
                        .collect();
                    assert_eq!(request.argv[1..], expected, "{provider} exact={exact}");
                    if provider == "fixture" {
                        assert!(
                            request.argv[0]
                                .to_string_lossy()
                                .ends_with("adapters/oms-agent-adapter-fixture")
                        );
                    } else {
                        assert_eq!(request.argv[0], bin);
                    }
                    assert_eq!(
                        request.input.is_some(),
                        [
                            "codex",
                            "claude",
                            "antigravity",
                            "cursor",
                            "grok",
                            "gemini",
                            "qwen",
                            "opencode",
                            "fixture"
                        ]
                        .contains(&provider),
                        "{provider}"
                    );
                    let overrides: Vec<_> = request
                        .environment
                        .iter()
                        .filter(|(k, _)| {
                            [
                                "DSH_PERMISSION_MODE",
                                "DSH_TELEMETRY_DISABLED",
                                "PI_SKIP_VERSION_CHECK",
                                "GOOSE_MODE",
                                "AUGMENT_DISABLE_AUTO_UPDATE",
                            ]
                            .iter()
                            .any(|n| k == n)
                        })
                        .cloned()
                        .collect();
                    let expected = match provider {
                        "deepseek" => vec![
                            ("DSH_PERMISSION_MODE".into(), "read-only".into()),
                            ("DSH_TELEMETRY_DISABLED".into(), "1".into()),
                        ],
                        "pi" => vec![("PI_SKIP_VERSION_CHECK".into(), "1".into())],
                        "goose" => vec![("GOOSE_MODE".into(), "auto".into())],
                        "auggie" => vec![("AUGMENT_DISABLE_AUTO_UPDATE".into(), "1".into())],
                        _ => vec![],
                    };
                    assert_eq!(overrides, expected, "{provider}");
                    if let Some(path) = &call.scratch {
                        assert!(!path.exists(), "scratch cleanup {provider}");
                    }
                }
            }
        },
    );
}
fn run(repo: &Repo, source: &Path, tool: &str, values: &[&str]) -> Output {
    run_with_source(tool, &args(values), &repo.0, source, &STOP)
}

#[test]
fn plan_delegate_uses_exact_issued_lease_and_native_verified_result() {
    fn isolated_resume_states(name: &str, body: impl Fn()) {
        for (layout, env) in [
            (
                "private-resume",
                &[("OMS_RESUME_SESSION", "fixture-ambient-resume")][..],
            ),
            ("private-empty-resume", &[("OMS_RESUME_SESSION", "")]),
            ("private", &[]),
        ] {
            provider_isolated_with_env(name, layout, env, &body);
        }
    }
    isolated_resume_states(
        "plan_delegate_uses_exact_issued_lease_and_native_verified_result",
        || {
            use oms_command_peer::process::{PhysicalAttempt, PhysicalContract, RegisterPhysical};
            use oms_command_plan::{Action, Context as PlanContext, Plan, Request, json};
            const SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
            fn git_at(repo: &Path, args: &[&str]) -> String {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args([
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                    ])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                String::from_utf8(output.stdout).unwrap().trim().to_owned()
            }
            fn drift_worktree(mode: &str, repo: &Path) {
                if mode.contains("-index-") {
                    fs::write(repo.join("tracked.txt"), "staged fixture change\n").unwrap();
                    git_at(repo, &["add", "--", "tracked.txt"]);
                } else {
                    git_at(repo, &["commit", "--allow-empty", "-qm", "concurrent head"]);
                }
            }
            struct Host {
                repo: PathBuf,
                base: String,
                mode: &'static str,
                calls: AtomicU64,
                alias_during_start: Mutex<Option<PathBuf>>,
            }
            impl oms_command_plan::Services for Host {
                fn group_membership_resolver(
                    &self,
                ) -> Option<oms_command_plan::services::GroupResolver> {
                    let mut pending = self.alias_during_start.lock().unwrap();
                    if pending.is_some() {
                        let active = self.repo.join(oms_command_plan::storage::ACTIVE);
                        let lock = oms_lock::Lock::for_file(
                            &active,
                            oms_lock::Settings::from_env().unwrap(),
                        )
                        .unwrap();
                        // Plan's source Git setup requests the resolver under
                        // ACTIVE, after the checkout's initial generation was read.
                        match lock.try_acquire() {
                            Err(oms_lock::Error::Busy) => {
                                let worker = pending.take().unwrap();
                                if self.mode == "worktree-alias-under-start-lock" {
                                    self.replace_worker_with_alias(&worker);
                                } else {
                                    self.arm_late_git_change(&worker);
                                }
                            }
                            Ok(_) => {}
                            Err(error) => panic!("cannot sample canonical Start lock: {error}"),
                        }
                    }
                    None
                }
            }
            impl Host {
                fn arm_late_git_change(&self, worker: &Path) {
                    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
                    let bin = home.join("bin");
                    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                        .filter(|path| path != &bin)
                        .map(|path| path.join("git"))
                        .find(|path| path.is_file())
                        .unwrap();
                    fs::write(
                        home.join("fixture-start-drift"),
                        format!("{}\n{}\n", worker.display(), self.mode),
                    )
                    .unwrap();
                    fs::write(
                        home.join("fixture-real-git"),
                        real_git.as_os_str().as_encoded_bytes(),
                    )
                    .unwrap();
                    let shim = bin.join("git");
                    fs::write(&shim, r#"#!/bin/sh
set -eu
real_git=$(cat "$HOME/fixture-real-git")
spec="$HOME/fixture-start-drift"
if [ -f "$spec" ]; then
    { IFS= read -r worker; IFS= read -r mode; } < "$spec"
    previous=
    checkout=
    cached=0
    for arg do
        if [ "$previous" = -C ]; then checkout=$arg; fi
        if [ "$arg" = --cached ]; then cached=1; fi
        previous=$arg
    done
    if [ "$checkout" = "$worker" ] && [ "$cached" = 1 ]; then
        "$real_git" "$@"
        case "$mode" in
            worktree-head-after-start-proof)
                "$real_git" -C "$worker" -c user.name=Fixture -c user.email=fixture@example.invalid commit --allow-empty -qm 'late HEAD'
                ;;
            worktree-index-after-start-proof)
                printf 'late index\n' > "$worker/tracked.txt"
                "$real_git" -C "$worker" add -- tracked.txt
                ;;
            *) exit 81 ;;
        esac
        mv "$spec" "$spec.used"
        exit 0
    fi
fi
exec "$real_git" "$@"
"#).unwrap();
                    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
                }

                fn replace_worker_with_alias(&self, path: &Path) {
                    let other = path.with_file_name("other-worktree");
                    fs::rename(path, path.with_file_name("retained-worktree")).unwrap();
                    git_at(
                        &self.repo,
                        &[
                            "-c",
                            "core.hooksPath=/dev/null",
                            "worktree",
                            "add",
                            "--detach",
                            other.to_str().unwrap(),
                            &self.base,
                        ],
                    );
                    std::os::unix::fs::symlink(&other, path).unwrap();
                }
                fn plan(&self, request: Request) -> json::Value {
                    let context =
                        PlanContext::from_environment_at(&request, &self.repo, &self.repo, None)
                            .unwrap();
                    let plan = Plan::open(context, self, &STOP).unwrap();
                    let output = plan.execute(&request).unwrap();
                    if output.stdout.first() == Some(&b'{') {
                        json::parse(&output.stdout).unwrap()
                    } else {
                        json::Value::Null
                    }
                }
                fn replace(&self) -> json::Value {
                    let old = self.plan(Request::new(Action::Show).with("id", "t1"));
                    self.plan(
                        Request::new(Action::Release)
                            .with("id", "t1")
                            .with("lease-id", old.text("lease_id")),
                    );
                    self.plan(
                        Request::new(Action::Claim)
                            .with("id", "t1")
                            .with("provider", "claude")
                            .flag("json"),
                    )
                }
            }
            impl Executor for Host {
                fn plan_delegate_services(&self) -> Option<&dyn oms_command_plan::Services> {
                    Some(self)
                }
                fn with_physical_contract(
                    &self,
                    attempt: &PhysicalAttempt<'_>,
                    _: std::time::Instant,
                    _: &AtomicBool,
                    register: RegisterPhysical<'_>,
                ) -> oms_command_peer::Result<()> {
                    if self.mode == "replace-before-send" {
                        self.replace();
                    }
                    match self.mode {
                        "head-before-send" => drift_worktree(self.mode, &self.repo),
                        "worktree-head-before-send" | "worktree-index-before-send" => {
                            drift_worktree(self.mode, attempt.execution_repo);
                        }
                        _ => {}
                    }
                    register(PhysicalContract::default()).map(|_| ())
                }
                fn execute_provider(
                    &self,
                    mut request: Invocation,
                    _: &oms_command_telemetry::store::usage::Identity,
                    _: bool,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    if self.mode == "worktree-split-index" {
                        assert!(
                            !git_at(&request.cwd, &["rev-parse", "--shared-index-path"]).is_empty()
                        );
                    }
                    if matches!(
                        self.mode,
                        "source-head-at-spawn"
                            | "worktree-head-at-spawn"
                            | "worktree-index-at-spawn"
                            | "lease-at-spawn"
                    ) {
                        // The logical gate already succeeded. Exercise the same
                        // fence at the native supervisor immediately before spawn.
                        let replacement = match self.mode {
                            "lease-at-spawn" => {
                                let replacement = self.replace();
                                for repo in [&self.repo, &request.cwd] {
                                    assert_eq!(git_at(repo, &["rev-parse", "HEAD"]), self.base);
                                    assert!(
                                        git_at(repo, &["diff", "--cached", "--name-only"])
                                            .is_empty()
                                    );
                                }
                                Some(replacement)
                            }
                            "source-head-at-spawn" => {
                                drift_worktree("worktree-head-at-spawn", &self.repo);
                                None
                            }
                            mode => {
                                drift_worktree(mode, &request.cwd);
                                None
                            }
                        };
                        let marker = request.cwd.join("provider-started");
                        request.argv = args(&["/bin/sh", "-c", "touch provider-started"]);
                        request.input = None;
                        let result = Native.execute_with_plan_services(request, Some(self), stop);
                        assert!(!marker.exists(), "{} launched a provider", self.mode);
                        if let Some(replacement) = replacement {
                            let err = result.as_ref().expect_err("stale lease started provider");
                            assert!(
                                err.message
                                    .contains("Plan delegation task, lease, scope or base changed")
                            );
                            let current =
                                self.plan(Request::new(Action::EvidenceSnapshot).with("id", "t1"));
                            assert_eq!(current.text("state"), "claimed");
                            assert_eq!(current.text("lease_id"), replacement.text("lease_id"));
                            assert_eq!(
                                current.text("task_sha256"),
                                replacement.text("task_sha256")
                            );
                        }
                        return result;
                    }
                    self.execute(request, stop)
                }
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    if matches!(request.argv[0].to_str(), Some("git" | "bash")) {
                        if matches!(
                            self.mode,
                            "worktree-head-before-start"
                                | "worktree-index-before-start"
                                | "worktree-alias-before-start"
                                | "worktree-alias-under-start-lock"
                                | "worktree-head-after-start-proof"
                                | "worktree-index-after-start-proof"
                        ) && request
                            .argv
                            .windows(2)
                            .any(|pair| pair[0] == "worktree" && pair[1] == "add")
                        {
                            let path = PathBuf::from(&request.argv[request.argv.len() - 2]);
                            let result = Native.execute(request, stop)?;
                            assert_eq!(result.code, 0);
                            match self.mode {
                                "worktree-alias-before-start" => {
                                    self.replace_worker_with_alias(&path)
                                }
                                "worktree-alias-under-start-lock"
                                | "worktree-head-after-start-proof"
                                | "worktree-index-after-start-proof" => {
                                    *self.alias_during_start.lock().unwrap() = Some(path);
                                }
                                _ => drift_worktree(self.mode, &path),
                            }
                            return Ok(result);
                        }
                        return Native.execute(request, stop);
                    }
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        request.environment.iter().all(|(key, _)| !key
                            .to_string_lossy()
                            .starts_with("OMS_DELEGATE_PLAN_"))
                    );
                    let prompt = request
                        .input
                        .as_ref()
                        .map(|raw| String::from_utf8_lossy(raw).into_owned())
                        .unwrap_or_else(|| {
                            request
                                .argv
                                .iter()
                                .map(|part| part.to_string_lossy())
                                .collect::<Vec<_>>()
                                .join(" ")
                        });
                    assert!(prompt.contains("Required verification:"));
                    assert!(prompt.contains("Forbidden paths: denied.txt"));
                    let script = match self.mode {
                        "unknown-stderr-only" => {
                            Some("printf '%s\\n' 'provider capacity unavailable' >&2; exit 1")
                        }
                        "unknown-claude-open" => Some(concat!(
                            "printf 'changed\\n' > tracked.txt; printf '%s\\n' ",
                            "'{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}' ",
                            "'{\"type\":\"system\",\"subtype\":\"init\"}'",
                        )),
                        "ok-footer" => Some(concat!(
                            "printf 'changed\\n' > tracked.txt; printf '%s\\n' ",
                            "'{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}' ",
                            "'{\"type\":\"system\",\"subtype\":\"hook_response\",\"hook_event\":\"Stop\",\"outcome\":\"success\"}' ",
                            "'{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\"}}'",
                        )),
                        _ => None,
                    };
                    if let Some(script) = script {
                        let mut fixture = request;
                        fixture.argv = args(&["/bin/sh", "-c", script]);
                        fixture.input = None;
                        return Native.execute_with_plan_services(fixture, Some(self), stop);
                    }
                    fs::write(
                        request.cwd.join("tracked.txt"),
                        if self.mode == "repair" {
                            "repaired\n"
                        } else {
                            "changed\n"
                        },
                    )
                    .unwrap();
                    if self.mode == "forbidden" {
                        fs::write(request.cwd.join("denied.txt"), "forbidden edit\n").unwrap();
                    }
                    if self.mode == "replace-on-return" {
                        self.replace();
                    }
                    Ok(Completed {
                        code: if self.mode == "failure" { 7 } else if self.mode == "cancel" { 130 } else { 0 },
                        stdout: b"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"implemented the frozen task\"}\n".to_vec(),
                        stderr: vec![], complete: self.mode != "cancel", cleanup_complete: Some(true),
                        owned_cleanup: None, elapsed: Duration::ZERO,
                    })
                }
            }
            let regular = [
                "ok",
                "foreign-provider",
                "foreign-model",
                "read-role",
                "forbidden",
                "replace-before-send",
                "head-before-send",
                "worktree-head-before-start",
                "worktree-index-before-start",
                "worktree-alias-before-start",
                "worktree-alias-under-start-lock",
                "worktree-head-after-start-proof",
                "worktree-index-after-start-proof",
                "worktree-split-index",
                "worktree-head-before-send",
                "worktree-index-before-send",
                "source-head-at-spawn",
                "worktree-head-at-spawn",
                "worktree-index-at-spawn",
                "lease-at-spawn",
                "unknown-stderr-only",
                "unknown-claude-open",
                "ok-footer",
                "replace-on-return",
                "failure",
                "cancel",
                "repair",
            ];
            let modes: &[&str] = match std::env::var("OMS_RESUME_SESSION").as_deref() {
                Ok("fixture-ambient-resume") => &["ambient-resume"],
                Ok("") => &["ok"],
                Err(std::env::VarError::NotPresent) => &regular,
                other => panic!("unexpected fixture resume state: {other:?}"),
            };
            for &mode in modes {
                let repo = provider_repo();
                repo.write("denied.txt", "unchanged\n");
                repo.write(".gitignore", ".oms/\n");
                repo.git(&["add", "denied.txt", ".gitignore"]);
                repo.git(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "scope",
                ]);
                let host = Host {
                    repo: repo.0.clone(),
                    base: git_at(&repo.0, &["rev-parse", "HEAD"]),
                    mode,
                    calls: AtomicU64::new(0),
                    alias_during_start: Mutex::new(None),
                };
                if mode == "worktree-split-index" {
                    git_at(&repo.0, &["config", "core.splitIndex", "true"]);
                }
                host.plan(Request::new(Action::Init).with("goal", "native Plan delegate"));
                let mut task_request = Request::new(Action::Add)
                    .with("id", "t1")
                    .with("title", "update tracked.txt")
                    .with("allowed", "tracked.txt,denied.txt")
                    .with("forbidden", "denied.txt")
                    .with("verify", "test -s tracked.txt")
                    .with(
                        "assignment",
                        if mode == "unknown-stderr-only" {
                            "{\"provider\":\"claude\",\"model\":\"fixture\",\"fallback_model\":\"fixture-fallback\"}"
                        } else {
                            "{\"provider\":\"claude\",\"model\":\"fixture\"}"
                        },
                    );
                if mode == "read-role" {
                    task_request = task_request.with("role", "researcher");
                }
                host.plan(task_request);
                let mut issued = host.plan(
                    Request::new(Action::Claim)
                        .with("id", "t1")
                        .with("provider", "claude")
                        .flag("json"),
                );
                let mut review_artifact = None;
                if mode == "repair" {
                    repo.write(".oms/artifacts/old.md", "prior real review\n");
                    repo.write(".oms/artifacts/old.patch", "prior patch\n");
                    host.plan(
                        Request::new(Action::Review)
                            .with("id", "t1")
                            .with("lease-id", issued.text("lease_id"))
                            .with("artifact", ".oms/artifacts/old.md")
                            .with("patch", ".oms/artifacts/old.patch"),
                    );
                    host.plan(
                        Request::new(Action::Repair)
                            .with("id", "t1")
                            .with("lease-id", issued.text("lease_id"))
                            .with("artifact", ".oms/artifacts/old.md"),
                    );
                    issued = host.plan(Request::new(Action::EvidenceSnapshot).with("id", "t1"));
                    review_artifact = Some(".oms/artifacts/old.md");
                }
                let context = PlanContext::from_environment_at(
                    &Request::new(Action::Status),
                    &repo.0,
                    &repo.0,
                    None,
                )
                .unwrap();
                let plan = Plan::open(context, &host, &STOP).unwrap();
                let binding = json::object([
                    ("schema", json::number(1)),
                    ("plan_id", issued.get("plan_id").clone()),
                    ("task_id", issued.get("id").clone()),
                    ("task_sha256", issued.get("task_sha256").clone()),
                    ("lease_id", issued.get("lease_id").clone()),
                    ("lease_epoch", issued.get("lease_epoch").clone()),
                    (
                        "base_sha",
                        json::string(plan.git().text(&["rev-parse", "HEAD"]).unwrap()),
                    ),
                ]);
                let binding = String::from_utf8(json::canonical(&binding).unwrap()).unwrap();
                let mut call = vec![
                    "--to".to_string(),
                    if mode == "foreign-provider" {
                        "codex"
                    } else {
                        "claude"
                    }
                    .into(),
                    "--model".into(),
                    if mode == "foreign-model" {
                        "other-model"
                    } else {
                        "fixture"
                    }
                    .into(),
                    "--plan-task".into(),
                    "t1".into(),
                    "--plan-binding".into(),
                    binding,
                ];
                if mode == "unknown-stderr-only" {
                    call.extend(["--fallback-model".into(), "fixture-fallback".into()]);
                }
                if let Some(path) = review_artifact {
                    call.extend([
                        "--terminalize-resumed-repair".into(),
                        "--review-artifact".into(),
                        path.into(),
                    ]);
                }
                let late_git_change = matches!(
                    mode,
                    "worktree-head-after-start-proof" | "worktree-index-after-start-proof"
                );
                let pre_start_refusal = late_git_change
                    || matches!(
                        mode,
                        "ambient-resume"
                            | "worktree-alias-before-start"
                            | "worktree-alias-under-start-lock"
                    );
                let events = oms_command_operations::event_path(&repo.0);
                let plan_file = repo.0.join(oms_command_plan::storage::ACTIVE);
                let before = pre_start_refusal.then(|| {
                    (
                        host.plan(Request::new(Action::EvidenceSnapshot).with("id", "t1")),
                        fs::read(&events).ok(),
                        fs::read(&plan_file).unwrap(),
                    )
                });
                let output = oms_command_peer::run_with_executor(
                    "peer-delegate",
                    &call.iter().map(OsString::from).collect::<Vec<_>>(),
                    &repo.0,
                    Path::new(SOURCE),
                    &STOP,
                    &host,
                );
                if late_git_change {
                    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
                    assert!(home.join("fixture-start-drift.used").is_file());
                    fs::remove_file(home.join("fixture-start-drift.used")).unwrap();
                    fs::remove_file(home.join("bin/git")).unwrap();
                }
                let task = host.plan(Request::new(Action::Show).with("id", "t1"));
                let message = format!(
                    "{mode}: {}{}",
                    String::from_utf8_lossy(&output.1),
                    String::from_utf8_lossy(&output.2)
                );
                if let Some((snapshot, prior_events, prior_plan)) = before {
                    let reason = if mode == "ambient-resume" {
                        "OMS_RESUME_SESSION cannot select a provider session for Plan delegation"
                    } else if late_git_change {
                        "Plan worker Git generation changed before Start"
                    } else {
                        "Plan worker checkout does not have its expected physical path"
                    };
                    assert_eq!(output.0, 2, "{message}");
                    assert!(message.contains(reason), "{message}");
                    assert_eq!(task.text("state"), "claimed", "{message}");
                    assert_eq!(
                        host.plan(Request::new(Action::EvidenceSnapshot).with("id", "t1"))
                            .get("task_sha256"),
                        snapshot.get("task_sha256"),
                        "{message}"
                    );
                    assert_eq!(fs::read(&events).ok(), prior_events);
                    assert_eq!(fs::read(&plan_file).unwrap(), prior_plan);
                    assert!(!repo.0.join(".oms/artifacts/index.jsonl").exists());
                    assert!(
                        !repo
                            .0
                            .join(oms_command_telemetry::store::usage::RECEIPTS)
                            .exists()
                    );
                    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
                    assert!(host.alias_during_start.lock().unwrap().is_none());
                    continue;
                }
                if matches!(mode, "ok" | "repair" | "ok-footer" | "worktree-split-index") {
                    assert_eq!(output.0, 0, "{message}");
                    assert_eq!(task.text("state"), "review", "{message}");
                    assert_eq!(task.text("review_lease_id"), issued.text("lease_id"));
                    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
                    assert_eq!(
                        fs::read_to_string(repo.0.join("tracked.txt")).unwrap(),
                        "baseline\n",
                        "owner checkout must remain unchanged"
                    );
                    let rows = oms_command_peer::storage::rows(
                        &fs::read(repo.0.join(".oms/artifacts/index.jsonl")).unwrap(),
                    )
                    .unwrap();
                    let row = rows
                        .iter()
                        .find(|row| row.get("kind").text() == Some("delegate"))
                        .unwrap();
                    assert_eq!(
                        row.get("plan_binding").get("lease_id").text(),
                        Some(issued.text("lease_id"))
                    );
                    assert_eq!(row.get("task_id").text(), Some("t1"));
                    for (path, digest) in
                        [("artifact", "artifact_sha256"), ("patch", "patch_sha256")]
                    {
                        assert_eq!(
                            row.get(digest).text().unwrap(),
                            json::sha(
                                fs::read(repo.0.join(row.get(path).text().unwrap())).unwrap()
                            )
                        );
                    }
                    if mode == "repair" {
                        assert!(task.get("repair_count").is_int(1));
                    }
                } else {
                    assert_ne!(output.0, 0, "{message}");
                    assert_ne!(task.text("state"), "review", "{message}");
                    assert_eq!(
                        host.calls.load(Ordering::SeqCst),
                        u64::from(!matches!(
                            mode,
                            "foreign-provider"
                                | "foreign-model"
                                | "read-role"
                                | "replace-before-send"
                                | "head-before-send"
                                | "worktree-head-before-start"
                                | "worktree-index-before-start"
                                | "worktree-head-before-send"
                                | "worktree-index-before-send"
                                | "source-head-at-spawn"
                                | "worktree-head-at-spawn"
                                | "worktree-index-at-spawn"
                                | "lease-at-spawn"
                        )),
                        "{message}"
                    );
                    if matches!(
                        mode,
                        "worktree-head-before-start" | "worktree-index-before-start"
                    ) {
                        assert_eq!(task.text("state"), "claimed", "{message}");
                        assert_eq!(task.text("lease_id"), issued.text("lease_id"));
                    }
                    if matches!(mode, "unknown-stderr-only" | "unknown-claude-open") {
                        let stderr = String::from_utf8_lossy(&output.2);
                        let worktree = stderr
                            .lines()
                            .find_map(|line| line.strip_prefix("worktree retained: "))
                            .unwrap_or_else(|| panic!("{message}"));
                        let expected = if mode == "unknown-claude-open" {
                            "changed\n"
                        } else {
                            "baseline\n"
                        };
                        assert_eq!(
                            fs::read_to_string(Path::new(worktree).join("tracked.txt")).unwrap(),
                            expected
                        );
                    }
                    if mode == "replace-on-return" {
                        assert_ne!(task.text("lease_id"), issued.text("lease_id"));
                        assert!(
                            fs::read_to_string(repo.0.join(".oms/plan/late-results.jsonl"))
                                .unwrap()
                                .contains("stale_delegate_binding")
                        );
                    }
                }
                let before_replay = host.calls.load(Ordering::SeqCst);
                let replay = oms_command_peer::run_with_executor(
                    "peer-delegate",
                    &call.iter().map(OsString::from).collect::<Vec<_>>(),
                    &repo.0,
                    Path::new(SOURCE),
                    &STOP,
                    &host,
                );
                assert_ne!(
                    replay.0, 0,
                    "replaying the old issued binding cannot resend: {mode}"
                );
                assert_eq!(host.calls.load(Ordering::SeqCst), before_replay);
            }
        },
    );
}
#[test]
fn provider_adapter_discovery_requires_marker() {
    for layout in ["override", "xdg", "home"] {
        provider_isolated("provider_adapter_discovery_requires_marker", layout, || {
            use oms_command_peer::providers::{binary, discovered, normalize};
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            let dir = match layout {
                "override" => home.join("adapters"),
                "xdg" => home.join("xdg/oh-my-setting/provider-adapters"),
                _ => home.join(".config/oh-my-setting/provider-adapters"),
            };
            assert_eq!(normalize("fixture").unwrap(), "fixture");
            assert_eq!(
                Path::new(&binary("fixture")),
                dir.join("oms-agent-adapter-fixture")
            );
            fs::set_permissions(
                dir.join("oms-agent-adapter-fixture"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert_eq!(binary("fixture"), "oms-agent-adapter-fixture");
            assert!(discovered("fixture"));
            fs::remove_file(home.join("bin/oms-agent-adapter-fixture")).unwrap();
            assert!(!discovered("fixture"));
            assert!(normalize("fixture").is_err());
            for name in ["phantom", "disabled"] {
                assert!(normalize(name).is_err(), "reject raw executable {name}");
                assert!(!discovered(name));
                assert_eq!(binary(name), format!("oms-agent-adapter-{name}"));
            }
            assert_eq!(normalize(&"a".repeat(64)).unwrap(), "a".repeat(64));
            for name in [
                "".into(),
                "a".repeat(65),
                ".bad".into(),
                "-bad".into(),
                "_bad".into(),
                "bad..id".into(),
                "Bad".into(),
                "a/b".into(),
                "a\0b".into(),
            ] {
                assert!(normalize(&name).is_err(), "reject malformed {name:?}");
                assert!(!discovered(&name));
            }
            assert_eq!(normalize("opencode2").unwrap(), "opencode");
            for (alias, provider) in [
                ("agy", "antigravity"),
                ("cursor-agent", "cursor"),
                ("grok-build", "grok"),
                ("gemini-cli", "gemini"),
                ("qwen-code", "qwen"),
                ("opencode2", "opencode"),
                ("dsh", "deepseek"),
                ("deepseek-harness", "deepseek"),
                ("mistral-vibe", "vibe"),
                ("github-copilot", "copilot"),
                ("factory-droid", "droid"),
                ("kiro-cli", "kiro"),
                ("cn", "continue"),
            ] {
                assert_eq!(normalize(alias).unwrap(), provider);
                assert_eq!(binary(alias), binary(provider));
                assert!(discovered(alias));
            }
            assert_eq!(binary("opencode"), "opencode2");
            assert!(discovered("opencode"));
            let primary = home.join("bin/opencode");
            fs::write(&primary, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&primary, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(binary("opencode"), "opencode");
            fs::remove_file(primary).unwrap();
            fs::remove_file(home.join("bin/opencode2")).unwrap();
            assert!(!discovered("opencode"));
        });
    }
}

#[test]
fn provider_private_prompt_cleanup_and_read_boundaries() {
    provider_isolated(
        "provider_private_prompt_cleanup_and_read_boundaries",
        "override",
        || {
            let repo = provider_repo();
            for provider in ["droid", "aider", "fixture", "antigravity"] {
                for outcome in [0, 7, -1] {
                    let probe = ProviderProbe {
                        outcome,
                        ..Default::default()
                    };
                    let output = provider_call(
                        &repo,
                        provider,
                        "provider-default",
                        "auto",
                        &vec![b'x'; 120001],
                        &probe,
                    );
                    if outcome == 0 {
                        success(output);
                    } else {
                        assert_ne!(output.0, 0);
                    }
                    let calls = probe.calls.lock().unwrap();
                    assert_eq!(calls.len(), 1, "{provider}");
                    let call = &calls[0];
                    assert!(call.prompt.len() > 120000);
                    assert!(!call.scratch.as_ref().unwrap().exists());
                }
            }
            for provider in ["aider", "fixture"] {
                let empty = Repo::new();
                let probe = ProviderProbe::default();
                assert_ne!(
                    provider_call(
                        &empty,
                        provider,
                        "provider-default",
                        "auto",
                        b"inspect",
                        &probe
                    )
                    .0,
                    0
                );
                assert!(
                    probe.calls.lock().unwrap().is_empty(),
                    "never execute in original checkout"
                );
                let probe = ProviderProbe {
                    mutate: true,
                    ..Default::default()
                };
                assert_ne!(
                    provider_call(
                        &repo,
                        provider,
                        "provider-default",
                        "auto",
                        b"inspect",
                        &probe
                    )
                    .0,
                    0
                );
                assert_eq!(fs::read(repo.0.join("tracked.txt")).unwrap(), b"baseline\n");
                let calls = probe.calls.lock().unwrap();
                assert_eq!(calls.len(), 1);
                assert_ne!(calls[0].request.cwd, repo.0);
                assert!(
                    calls[0].request.cwd.exists(),
                    "changed disposable worktree retained for inspection"
                );
                assert!(!calls[0].scratch.as_ref().unwrap().exists());
            }
            for prompt in [b"".as_slice(), &[255]] {
                success(provider_call(
                    &repo,
                    "pi",
                    "provider-default",
                    "auto",
                    prompt,
                    &ProviderProbe::default(),
                ));
            }
            let malformed = ProviderProbe::default();
            assert_ne!(
                provider_call(
                    &repo,
                    "pi",
                    "provider-default",
                    "auto",
                    b"bad\0prompt",
                    &malformed
                )
                .0,
                0
            );
            assert!(malformed.calls.lock().unwrap().is_empty());
            let probe = ProviderProbe::default();
            success(provider_call(
                &repo,
                "pi",
                "provider-default",
                "auto",
                b"x",
                &probe,
            ));
            let overhead = probe.calls.lock().unwrap()[0].prompt.len() - 1;
            for size in [1, 120000 - overhead, 120001 - overhead] {
                let probe = ProviderProbe::default();
                let output = provider_call(
                    &repo,
                    "pi",
                    "provider-default",
                    "auto",
                    &vec![b'x'; size],
                    &probe,
                );
                if size + overhead <= 120000 {
                    success(output);
                    assert_eq!(probe.calls.lock().unwrap()[0].prompt.len(), size + overhead);
                } else {
                    assert_ne!(output.0, 0);
                    assert!(probe.calls.lock().unwrap().is_empty());
                }
            }
            assert!(fs::read_dir(std::env::temp_dir()).unwrap().all(|entry| {
                let name = entry.unwrap().file_name();
                ![
                    "oms-droid-",
                    "oms-aider-",
                    "oms-adapter-",
                    "oms-agy-prompt-",
                ]
                .iter()
                .any(|prefix| name.to_string_lossy().starts_with(prefix))
            }));
        },
    );
}
fn success(output: Output) -> Vec<u8> {
    assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
    output.1
}
#[test]
fn ownership_and_help() {
    isolated("ownership_and_help", || {
        let repo = Repo::new();
        for tool in oms_command_peer::TOOLS {
            assert!(owns(tool, &args(&["--malformed"])));
            assert!(!success(run(&repo, &repo.0, tool, &["--help"])).is_empty());
        }
        assert!(!owns("room", &[]));
        assert_eq!(run(&repo, &repo.0, "unknown", &[]), (75, vec![], vec![]));
    });
}
#[test]
fn view_embedded_and_installed_assets() {
    isolated("view_embedded_and_installed_assets", || {
        let repo = Repo::new();
        let source = Repo::new();
        repo.write(
            "results.json",
            br#"{"schema":1,"kind":"oms-study-results","rows":[]}"#,
        );
        let build = |output: &str| {
            run(
                &repo,
                &source.0,
                "artifact-view",
                &[
                    "build",
                    "--input",
                    "results.json",
                    "--output",
                    output,
                    "--title",
                    "A < B",
                ],
            )
        };
        let meta = V::parse(&success(build("embedded.html"))).unwrap();
        assert_eq!(meta.get("path").text(), Some("embedded.html"));
        let html = fs::read_to_string(repo.0.join("embedded.html")).unwrap();
        assert!(html.contains("A &lt; B"));
        for asset in [
            include_str!("../assets/analysis.js"),
            include_str!("../assets/view.js"),
            include_str!("../assets/view.css"),
        ] {
            assert!(html.contains(asset));
        }
        for (name, body) in [
            ("analysis.js", include_str!("../assets/analysis.js")),
            ("view.js", include_str!("../assets/view.js")),
            ("view.css", include_str!("../assets/view.css")),
            ("view.html", include_str!("../assets/view.html")),
        ] {
            oms_command_peer::safety::outbound(body.as_bytes()).unwrap();
            source.write(&format!("scripts/lib/artifact_view_assets/{name}"), body);
        }
        success(build("installed.html"));
        assert_eq!(
            fs::read_to_string(repo.0.join("installed.html")).unwrap(),
            html
        );
        fs::remove_dir_all(source.0.join("scripts/lib/artifact_view_assets")).unwrap();
        source.write(
            "scripts/lib/artifact_view_assets/view.css",
            "/* installed override */",
        );
        success(build("mixed.html"));
        assert!(
            fs::read_to_string(repo.0.join("mixed.html"))
                .unwrap()
                .contains("/* installed override */")
        );
        source.write("scripts/lib/artifact_view_assets/view.css", "");
        success(build("empty.html"));
        assert!(
            !fs::read_to_string(repo.0.join("empty.html"))
                .unwrap()
                .contains(include_str!("../assets/view.css"))
        );
        source.write("scripts/lib/artifact_view_assets/view.css", [255]);
        assert_eq!(build("invalid.html").0, 2);
        assert!(!repo.0.join("invalid.html").exists());
        fs::remove_file(source.0.join("scripts/lib/artifact_view_assets/view.css")).unwrap();
        std::os::unix::fs::symlink(
            source.0.join("missing"),
            source.0.join("scripts/lib/artifact_view_assets/view.css"),
        )
        .unwrap();
        assert_eq!(build("linked.html").0, 2);
        assert!(!repo.0.join("linked.html").exists());
        fs::remove_dir_all(source.0.join("scripts/lib/artifact_view_assets")).unwrap();
        std::os::unix::fs::symlink(
            source.0.join("absent"),
            source.0.join("scripts/lib/artifact_view_assets"),
        )
        .unwrap();
        assert_eq!(build("ancestor.html").0, 2);
        assert!(!repo.0.join("ancestor.html").exists());
    });
}
#[test]
fn peer_dry_runs_and_ml_context() {
    isolated("peer_dry_runs_and_ml_context", || {
        let repo = Repo::new();
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ]);
        for tool in ["peer-ask", "peer-delegate"] {
            let values = [
                if tool == "peer-ask" {
                    "--providers"
                } else {
                    "--to"
                },
                "codex",
                "--model",
                "fixture",
                "--dry-run",
                "--no-verify",
                "Inspect behavior",
            ];
            success(run(&repo, &repo.0, tool, &values));
            assert_eq!(run(&repo, &repo.0, tool, &["--to"]).0, 2);
        }
        repo.write("train.py", "import torch\n");
        success(run(
            &repo,
            &repo.0,
            "peer-ask",
            &[
                "--providers",
                "codex",
                "--model",
                "fixture",
                "--dry-run",
                "--ml-context",
                "Inspect training",
            ],
        ));
        let prompts = fs::read_dir(repo.0.join(".oms/artifacts/ask"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(prompts.contains("ML Agent Context Digest"));
        assert!(prompts.contains("train.py"));
        repo.write(
            "src/lib.rs",
            "pub fn graph_context_probe() -> usize { 42 }\n",
        );
        repo.git(&["add", "src/lib.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "add graph probe",
        ]);
        let graph_out = run(
            &repo,
            &repo.0,
            "peer-delegate",
            &[
                "--to",
                "codex",
                "--model",
                "fixture",
                "--dry-run",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--graph-context",
                "Inspect graph_context_probe",
            ],
        );
        assert_eq!(graph_out.0, 0, "{}", String::from_utf8_lossy(&graph_out.2));
        let delegate_prompts = fs::read_dir(repo.0.join(".oms/artifacts/delegate"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            delegate_prompts.contains("project graph")
                && delegate_prompts.contains("graph_context_probe"),
            "missing injected graph context in delegate prompt:\n{delegate_prompts}"
        );
        assert!(delegate_prompts.contains("delivery_status"));
        assert!(delegate_prompts.contains("worker_snapshot_bound"));
        if let Ok(manifest) = fs::read_to_string(repo.0.join(".oms/project-graph/manifest.json")) {
            let manifest = oms_command_operations::value::V::parse(manifest.as_bytes()).unwrap();
            assert!(
                !manifest
                    .get("discovery")
                    .get("exclude")
                    .array()
                    .iter()
                    .any(|v| v.text() == Some("*.py"))
            );
        }
        for (option, message) in [
            ("--apply", "patch-admission"),
            ("--plan-task", "plan lease"),
        ] {
            let mut values = vec![option];
            if option == "--plan-task" {
                values.push("T1");
            }
            let output = run(&repo, &repo.0, "peer-delegate", &values);
            assert_eq!(output.0, 2);
            assert!(String::from_utf8_lossy(&output.2).contains(message));
        }
    });
}
#[test]
fn review_classification_and_validation() {
    provider_isolated("review_classification_and_validation", "override", || {
        let repo = Repo::new();
        let artifact = "review/codex-20260101T000000Z-1.md";
        for (answer, exit, expected, state) in [
            ("GATE: pass", "0", 0, "pass"),
            ("GATE: fail", "0", 1, "fail"),
            ("GATE: pass", "1", 2, "incomplete"),
            ("", "0", 2, "incomplete"),
            ("GATE: pass", "bad", 2, "incomplete"),
        ] {
            repo.write(artifact, format!("## Output\n{answer}\n## Exit\n{exit}\n"));
            let output = run(
                &repo,
                &repo.0,
                "peer-review",
                &["verdicts", "review", "--json"],
            );
            assert_eq!(output.0, expected);
            let value = V::parse(&output.1).unwrap();
            assert_eq!(
                value.get("seats").array()[0].get("verdict").text(),
                Some(state)
            );
        }
        for option in ["--verify", "--covers", "--confirm", "--security-audit"] {
            let values = [option, "fixture"];
            let output = run(&repo, &repo.0, "peer-review", &values);
            assert_eq!(output.0, 2);
            assert!(String::from_utf8_lossy(&output.2).contains("requires --gate"));
        }

        struct ReviewProbe<'a> {
            repo: &'a Repo,
            answer: &'a str,
            mutation: u8,
            diffs: AtomicU64,
        }
        impl Executor for ReviewProbe<'_> {
            fn execute(
                &self,
                request: Invocation,
                stop: &AtomicBool,
            ) -> oms_command_peer::Result<Completed> {
                if request.argv[0] == "git" {
                    if request.argv.iter().any(|arg| arg == "--no-ext-diff") {
                        let number = self.diffs.fetch_add(1, Ordering::Relaxed) + 1;
                        if self.mutation == 2 && number == 4 {
                            self.repo
                                .write("source.rs", "changed after citation reads\n");
                        }
                    }
                    return Native.execute(request, stop);
                }
                if self.mutation == 1 {
                    self.repo
                        .write("source.rs", "changed during provider call\n");
                } else if self.mutation == 3 {
                    self.repo.git(&[
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                        "commit",
                        "--allow-empty",
                        "-qm",
                        "changed review head",
                    ]);
                }
                Ok(Completed {
                    code: 0,
                    stdout: self.answer.as_bytes().to_vec(),
                    stderr: Vec::new(),
                    complete: true,
                    cleanup_complete: Some(true),
                    owned_cleanup: None,
                    elapsed: Duration::ZERO,
                })
            }
        }
        use oms_command_peer::{obj, storage};
        let repo = provider_repo();
        let mut lines = (1..=40).map(|n| format!("// line {n}")).collect::<Vec<_>>();
        for (index, line) in [
            (13, "context_only();"),
            (19, "multi("),
            (20, "    second );"),
            (24, "repeated();"),
            (26, "repeated();"),
            (29, "twice(); twice();"),
            (31, "++++"),
            (35, "outside();"),
        ] {
            lines[index] = line.into();
        }
        lines[37] = "x".repeat(500);
        repo.write("source.rs", format!("{}\n", lines.join("\n")));
        repo.write("binary.rs", b"invalid \xff source\n");
        std::os::unix::fs::symlink("source.rs", repo.0.join("link.rs")).unwrap();
        repo.git(&["add", "source.rs", "binary.rs", "link.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "source anchors",
        ]);
        lines[14] = "changed();".into();
        let body = format!("{}\n", lines.join("\n"));
        repo.write("source.rs", &body);
        repo.write(".oms/extra.rs", "extra();\n");
        let finding = |file: &str, line: usize, summary: &str, quote: &str| {
            obj([
                ("file", file.into()),
                ("line", line.into()),
                ("summary", summary.into()),
                ("severity", "blocking".into()),
                ("quote", quote.into()),
            ])
        };
        let cases = [
            ("far", 2, "changed();", Some(15), "relocated", "in-diff"),
            ("near", 14, "changed();", Some(15), "relocated", "in-diff"),
            ("exact", 15, "changed();", Some(15), "verified", "in-diff"),
            (
                "context",
                1,
                "context_only();",
                Some(14),
                "relocated",
                "context",
            ),
            (
                "outside",
                15,
                "outside();",
                Some(36),
                "relocated",
                "outside-scope",
            ),
            (
                "out of range",
                900,
                "changed();",
                Some(15),
                "relocated",
                "in-diff",
            ),
            (
                "multiline",
                2,
                "multi(\n\tsecond );",
                Some(20),
                "relocated",
                "outside-scope",
            ),
            (
                "duplicate",
                25,
                "repeated();",
                None,
                "ambiguous",
                "anchor-not-found",
            ),
            (
                "same line",
                30,
                "twice();",
                None,
                "ambiguous",
                "anchor-not-found",
            ),
            (
                "overlapping",
                32,
                "+++",
                None,
                "ambiguous",
                "anchor-not-found",
            ),
            (
                "missing",
                15,
                "absent();",
                None,
                "not-found",
                "anchor-not-found",
            ),
            (
                "prefix",
                15,
                "changed(); missing suffix",
                None,
                "not-found",
                "anchor-not-found",
            ),
            (
                "token prefix",
                15,
                "change",
                None,
                "not-found",
                "anchor-not-found",
            ),
            (
                "case",
                15,
                "CHANGED();",
                None,
                "not-found",
                "anchor-not-found",
            ),
            ("no quote", 15, "", None, "no-quote", "in-diff"),
            (
                "whitespace quote",
                15,
                " \n\t ",
                None,
                "no-quote",
                "in-diff",
            ),
            (
                "no quote missing line",
                900,
                "",
                None,
                "not-found",
                "anchor-not-found",
            ),
        ];
        let mut claims = cases
            .iter()
            .map(|(summary, line, quote, ..)| finding("source.rs", *line, summary, quote))
            .collect::<Vec<_>>();
        claims.extend([
            finding("source.rs", 38, "truncated quote", &"x".repeat(501)),
            finding("source.rs", 15, "stripped control", "changed\0();"),
            finding("source.rs", 15, "control-only quote", "\0"),
            finding("link.rs", 15, "symlink", "changed();"),
            finding("binary.rs", 1, "invalid utf8", "source"),
            finding(".oms/extra.rs", 1, "untracked", "extra();"),
            finding("../source.rs", 15, "traversal", "changed();"),
        ]);
        let answer = format!(
            "```findings\n{}\n```\nCONFIDENCE: 0.8\nGATE: pass",
            claims
                .iter()
                .map(|row| String::from_utf8(row.json(false).unwrap()).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let review = |answer: &str, directory: &str, mutation, no_diff| {
            let probe = ReviewProbe {
                repo: &repo,
                answer,
                mutation,
                diffs: AtomicU64::new(0),
            };
            let mut values = vec![
                "--providers",
                "codex",
                "--model",
                "fixture",
                "--gate",
                "--prompt",
                "inspect source anchors",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--no-graph-context",
                "--artifact-dir",
                directory,
            ];
            if no_diff {
                values.extend(["--no-diff", "--verify", "true"]);
            } else {
                values.push("--no-verify");
            }
            let output = oms_command_peer::run_with_executor(
                "peer-review",
                &args(&values),
                &repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                &STOP,
                &probe,
            );
            assert!(output.0 == 0 || output.0 == 2, "{output:?}");
            let name = storage::names(&repo.0, Path::new(directory), ".jsonl")
                .unwrap()
                .into_iter()
                .find(|name| name.starts_with("_findings-"))
                .unwrap_or_else(|| panic!("missing findings: {output:?}"));
            let rows = storage::load(&repo.0, &Path::new(directory).join(name)).unwrap();
            (output, rows)
        };
        let (output, rows) = review(&answer, ".oms/review-anchors", 0, false);
        assert_eq!(output.0, 0, "anchor labels never change GATE: pass");
        assert_eq!(rows.len(), claims.len() - 1, "traversal must be rejected");
        for ((summary, reported, quote, resolved, status, anchor), row) in cases.iter().zip(&rows) {
            assert_eq!(row.get("summary").text(), Some(*summary));
            assert_eq!(row.get("reported_line"), &V::from(*reported));
            assert_eq!(row.get("line"), &V::from(resolved.unwrap_or(*reported)));
            assert_eq!(row.get("resolved_line"), &resolved.map_or(V::Null, V::from));
            assert_eq!(
                row.get("line_anchor_status").text(),
                Some(*status),
                "{summary}"
            );
            assert_eq!(row.get("anchor").text(), Some(*anchor), "{summary}");
            assert_eq!(
                row.get("quote_found"),
                &if quote.trim().is_empty() {
                    V::Null
                } else {
                    resolved.is_some().into()
                }
            );
            assert_eq!(
                row.get("source_sha256"),
                &if resolved.is_some() {
                    storage::hash(body.as_bytes()).into()
                } else {
                    V::Null
                }
            );
        }
        for (row, status) in rows[cases.len()..].iter().zip([
            "incomplete-quote",
            "incomplete-quote",
            "incomplete-quote",
            "unavailable-source",
            "unavailable-source",
            "untracked-source",
        ]) {
            assert_eq!(row.get("line_anchor_status").text(), Some(status));
            assert_eq!(row.get("resolved_line"), &V::Null);
            assert_eq!(row.get("quote_found"), &false.into());
        }
        let one = |line| {
            format!(
                "```findings\n{}\n```\nGATE: pass",
                String::from_utf8(
                    finding("source.rs", line, "far", "changed();")
                        .json(false)
                        .unwrap()
                )
                .unwrap()
            )
        };
        let (output, no_diff) = review(&one(39), ".oms/review-no-diff", 0, true);
        assert_eq!(output.0, 0);
        assert_eq!(no_diff[0].get("anchor").text(), Some("unclassified"));
        assert_eq!(no_diff[0].get("resolved_line"), &15usize.into());
        assert_eq!(
            no_diff[0].get("id"),
            rows[0].get("id"),
            "line relocation preserves finding IDs"
        );
        for mutation in [1, 2, 3] {
            repo.write("source.rs", &body);
            let (output, drifted) = review(
                &one(2),
                &format!(".oms/review-drift-{mutation}"),
                mutation,
                false,
            );
            assert_eq!(output.0, 2, "source drift must make review incomplete");
            let row = &drifted[0];
            assert_eq!(row.get("line_anchor_status").text(), Some("source-changed"));
            assert_eq!(row.get("line"), &2usize.into());
            assert_eq!(row.get("resolved_line"), &V::Null);
            assert_eq!(row.get("source_sha256"), &V::Null);
            assert_eq!(row.get("quote_found"), &false.into());
            assert_eq!(row.get("anchor").text(), Some("anchor-not-found"));
        }

        // Account-specific council seats on one provider stay distinct during verdict reconstruction.
        let seat_a = "review-multi/codex-call-111111111111-20260102T000000Z-2.md";
        let seat_b = "review-multi/codex-call-222222222222-20260102T000000Z-2.md";
        repo.write(seat_a, "## Output\nGATE: fail\n## Exit\n0\n");
        repo.write(seat_b, "## Output\nGATE: pass\n## Exit\n0\n");
        let multi = run(
            &repo,
            &repo.0,
            "peer-review",
            &["verdicts", "review-multi", "--json"],
        );
        assert_eq!(multi.0, 1);
        let parsed = V::parse(&multi.1).unwrap();
        let multi_seats = parsed.get("seats").array();
        assert_eq!(multi_seats.len(), 2);
        assert_eq!(multi_seats[0].get("verdict").text(), Some("fail"));
        assert_eq!(multi_seats[1].get("verdict").text(), Some("pass"));

        repo.write(
            "review-multi/codex-call-111111111111-20260102T000000Z-2-r1.md",
            "## Output\nGATE: pass\n## Exit\n0\n",
        );
        let updated = run(
            &repo,
            &repo.0,
            "peer-review",
            &["verdicts", "review-multi", "--json"],
        );
        assert_eq!(updated.0, 0);
        let updated_parsed = V::parse(&updated.1).unwrap();
        let updated_seats = updated_parsed.get("seats").array();
        assert_eq!(updated_seats.len(), 2);
        assert_eq!(updated_seats[0].get("round").py(), "1");

        let indexed = storage::load(&repo.0, Path::new(".oms/artifacts/index.jsonl")).unwrap();
        for kind in ["review", "review-verify", "review-outcome"] {
            assert!(
                indexed
                    .iter()
                    .any(|row| row.get("kind").text() == Some(kind))
            );
        }
        let validation = oms_command_telemetry::run(&args(&["validate"]), &repo.0, &STOP);
        assert_eq!(
            validation.0,
            0,
            "{}{}",
            String::from_utf8_lossy(&validation.1),
            String::from_utf8_lossy(&validation.2)
        );

        struct GateVerifyProbe<'a> {
            repo: &'a Repo,
            provider_calls: AtomicU64,
            verify_commands: Mutex<Vec<String>>,
            strip_fast_during_call: bool,
            answer: Vec<u8>,
        }
        impl Executor for GateVerifyProbe<'_> {
            fn execute(
                &self,
                request: Invocation,
                stop: &AtomicBool,
            ) -> oms_command_peer::Result<Completed> {
                if request.argv[0] == "git" {
                    return Native.execute(request, stop);
                }
                if request.argv[0] == "bash" {
                    if let Some(cmd) = request.argv.last() {
                        self.verify_commands
                            .lock()
                            .unwrap()
                            .push(cmd.to_string_lossy().into_owned());
                    }
                    return Ok(Completed {
                        code: 0,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        complete: true,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::ZERO,
                    });
                }
                self.provider_calls.fetch_add(1, Ordering::SeqCst);
                if self.strip_fast_during_call {
                    self.repo
                        .write("scripts/check.sh", "#!/usr/bin/env bash\nexit 0\n");
                }
                Ok(Completed {
                    code: 0,
                    stdout: self.answer.clone(),
                    stderr: Vec::new(),
                    complete: true,
                    cleanup_complete: Some(true),
                    owned_cleanup: None,
                    elapsed: Duration::ZERO,
                })
            }
        }
        let gate_repo = provider_repo();
        let provider_answer = std::cell::RefCell::new(b"CONFIDENCE: 0.9\nGATE: pass\n".to_vec());
        let commit_check_sh = |script: &str, message: &str| {
            gate_repo.write("scripts/check.sh", script);
            gate_repo.git(&["add", "scripts/check.sh"]);
            gate_repo.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                message,
            ]);
        };
        let run_gate = |extra: &[&str], directory: &str, strip_fast: bool| {
            let probe = GateVerifyProbe {
                repo: &gate_repo,
                provider_calls: AtomicU64::new(0),
                verify_commands: Mutex::new(Vec::new()),
                strip_fast_during_call: strip_fast,
                answer: provider_answer.borrow().clone(),
            };
            let mut values = vec![
                "--providers",
                "codex",
                "--model",
                "fixture",
                "--gate",
                "--prompt",
                "check gate verify resolution",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--no-graph-context",
                "--artifact-dir",
                directory,
            ];
            values.extend_from_slice(extra);
            let output = oms_command_peer::run_with_executor(
                "peer-review",
                &args(&values),
                &gate_repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                &STOP,
                &probe,
            );
            (
                output,
                probe.provider_calls.load(Ordering::SeqCst),
                probe.verify_commands.into_inner().unwrap(),
            )
        };

        commit_check_sh(
            "#!/usr/bin/env bash\nexit 0\n",
            "check.sh without fast mode",
        );
        let (output, calls, verified) = run_gate(&[], ".oms/review-gate-no-fast", false);
        assert_eq!(output.0, 2);
        assert!(
            String::from_utf8_lossy(&output.2)
                .contains("check.sh has no declared fast mode; give --verify CMD or --no-verify"),
            "{output:?}"
        );
        assert_eq!(
            calls, 0,
            "unresolvable gate verify must fail before starting any provider seat"
        );
        assert!(verified.is_empty());

        let (output, calls, verified) = run_gate(
            &["--verify", "true", "--no-verify"],
            ".oms/review-gate-conflict",
            false,
        );
        assert_eq!(output.0, 2);
        assert!(
            String::from_utf8_lossy(&output.2)
                .contains("--verify and --no-verify are mutually exclusive"),
            "{output:?}"
        );
        assert_eq!(calls, 0);
        assert!(verified.is_empty());

        let (output, calls, verified) =
            run_gate(&["--verify", "echo ok"], ".oms/review-gate-explicit", false);
        assert_eq!(output.0, 0, "{output:?}");
        assert_eq!(calls, 1);
        assert_eq!(verified, vec!["echo ok".to_owned()]);

        let (output, calls, verified) =
            run_gate(&["--no-verify"], ".oms/review-gate-no-verify", false);
        assert_eq!(output.0, 0, "{output:?}");
        assert_eq!(calls, 1);
        assert!(verified.is_empty());

        commit_check_sh(
            "#!/usr/bin/env bash\ncase \"${1:-}\" in\n  fast) exit 0 ;;\nesac\n",
            "check.sh with fast mode",
        );
        let (output, calls, verified) = run_gate(&[], ".oms/review-gate-fast", false);
        assert_eq!(output.0, 0, "{output:?}");
        assert_eq!(calls, 1);
        assert_eq!(verified, vec!["bash scripts/check.sh fast".to_owned()]);

        // Reusing the pre-resolved verify command must not re-parse scripts/check.sh after reviews.
        let (output, calls, verified) = run_gate(&[], ".oms/review-gate-reused", true);
        assert_eq!(output.0, 2, "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.2).contains("no declared fast mode"),
            "{output:?}"
        );
        assert_eq!(calls, 1);
        assert_eq!(verified, vec!["bash scripts/check.sh fast".to_owned()]);

        // #491: a prose key-marker mention with no END line redacts the rest of the answer; the
        // verdict read from the raw answer is stored in the artifact header, so the gate keeps it.
        *provider_answer.borrow_mut() = format!(
            "The finding quotes a {}{} PRIVATE KEY{} line from a fixture.\nCONFIDENCE: 0.7\nGATE: pass\n",
            "-----", "BEGIN", "-----"
        )
        .into_bytes();
        let directory = ".oms/review-gate-redacted-verdict";
        let (output, calls, _) = run_gate(&["--no-verify"], directory, false);
        assert_eq!(output.0, 0, "{output:?}");
        assert_eq!(calls, 1);
        let saved = std::fs::read_dir(gate_repo.0.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "md")
                    && !path.file_name().unwrap().to_string_lossy().starts_with('_')
            })
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect::<String>();
        assert!(saved.contains("[oms-redacted:private-key]"), "{saved}");
        assert!(!saved.contains("from a fixture"), "{saved}");
        assert!(
            saved.contains("- verdict: pass\n- confidence: 0.7\n"),
            "{saved}"
        );

        // A quoted earlier pass must not outrank the redacted final fail, and a refusal, truncation
        // or empty answer hidden behind the marker must not turn into a pass.
        let marker = format!("{}{} PRIVATE KEY{}", "-----", "BEGIN", "-----");
        for (answer, directory, expected) in [
            (
                format!(
                    "Quoted: GATE: pass\nGATE: pass\nSee {marker} here.\nCONFIDENCE: 0.6\nGATE: fail\n"
                ),
                ".oms/review-gate-redacted-fail",
                1,
            ),
            (
                format!(
                    "See {marker} here.\nCONFIDENCE: 0.9\nGATE: pass\nstop-reason: provider=codex reason=refusal subtype=none is_error=0\n"
                ),
                ".oms/review-gate-redacted-refusal",
                2,
            ),
            (
                format!(
                    "CONFIDENCE: 0.9\nGATE: pass\nSee {marker} here.\nstop-reason: provider=codex reason=stream_truncated subtype=none is_error=0\n"
                ),
                ".oms/review-gate-redacted-truncated",
                2,
            ),
            (
                format!("served model\nGATE: pass\nsession id: {marker}\n"),
                ".oms/review-gate-redacted-empty",
                2,
            ),
        ] {
            *provider_answer.borrow_mut() = answer.into_bytes();
            let (output, calls, _) = run_gate(&["--no-verify"], directory, false);
            assert_eq!(output.0, expected, "{directory}: {output:?}");
            assert_eq!(calls, 1);
            let state = if expected == 1 { "fail" } else { "incomplete" };
            let listed = run(
                &gate_repo,
                &gate_repo.0,
                "peer-review",
                &["verdicts", directory, "--json"],
            );
            let listed = V::parse(&listed.1).unwrap();
            assert_eq!(
                listed.get("seats").array()[0].get("verdict").text(),
                Some(state),
                "{directory}"
            );
        }
    });
}
#[test]
fn review_gate_timeout_and_pinned_base() {
    let name = "review_gate_timeout_and_pinned_base";
    let git_stdout = |repo: &Repo, git_args: &[&str]| -> String {
        let mut command = Command::new("git");
        let output = clean_env_child(&mut command)
            .arg("-C")
            .arg(&repo.0)
            .args(git_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    // #457: real wall-clock timeout via OMS_PEER_TIMEOUT=1s must be reported as timed out,
    // never as interrupted, even when the timed-out child prints "error: interrupted" on SIGTERM.
    provider_isolated_with_env(name, "timeout-1s", &[("OMS_PEER_TIMEOUT", "1s")], || {
        use oms_command_peer::storage;
        let repo = provider_repo();
        repo.write("lib.rs", "pub fn answer() -> i32 { 1 }\n");
        repo.git(&["add", "lib.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "base",
        ]);
        let base_sha = git_stdout(&repo, &["rev-parse", "HEAD"]);
        repo.write("lib.rs", "pub fn answer() -> i32 { 2 }\n");
        let diff_bytes = Command::new("git")
            .arg("-C")
            .arg(&repo.0)
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "HEAD",
                "--",
            ])
            .output()
            .unwrap()
            .stdout
            .len();
        assert!(diff_bytes > 0);

        struct SlowSeatExecutor;
        impl Executor for SlowSeatExecutor {
            fn execute(
                &self,
                mut request: Invocation,
                stop: &AtomicBool,
            ) -> oms_command_peer::Result<Completed> {
                if request.argv[0] != "git" {
                    request.argv = vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "trap 'printf \"error: interrupted\\n\" >&2; exit 130' TERM INT; sleep 30"
                            .into(),
                    ];
                }
                Native.execute(request, stop)
            }
        }

        let dir = ".oms/review-timeout-1s";
        let output = oms_command_peer::run_with_executor(
            "peer-review",
            &args(&[
                "--providers",
                "codex",
                "--model",
                "fixture",
                "--gate",
                "--no-verify",
                "--prompt",
                "check timeout reporting",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--no-graph-context",
                "--artifact-dir",
                dir,
            ]),
            &repo.0,
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &STOP,
            &SlowSeatExecutor,
        );
        assert_eq!(output.0, 2, "{output:?}");
        let stdout = String::from_utf8_lossy(&output.1);
        assert!(
            stdout.contains("codex: timed out after 1s"),
            "stdout must report wall-clock timeout: {stdout}"
        );
        assert!(
            stdout.contains(&format!("diff {diff_bytes} bytes")),
            "stdout must record prompt diff size: {stdout}"
        );
        assert!(
            stdout.contains("elapsed "),
            "stdout must record elapsed seconds: {stdout}"
        );
        assert!(
            !stdout.to_lowercase().contains("interrupted"),
            "timeout stdout must not say interrupted: {stdout}"
        );

        let seat_name = storage::names(&repo.0, Path::new(dir), ".md")
            .unwrap()
            .into_iter()
            .find(|n| n.starts_with("codex-"))
            .expect("seat artifact");
        let artifact = String::from_utf8(
            storage::read(&repo.0, &Path::new(dir).join(&seat_name), false).unwrap(),
        )
        .unwrap();
        assert!(
            artifact.contains("error: timed out after 1s"),
            "artifact diagnostics must record timeout: {artifact}"
        );
        assert!(
            !artifact.contains("error: interrupted"),
            "timeout artifact must not record error: interrupted: {artifact}"
        );

        let verdicts_name = storage::names(&repo.0, Path::new(dir), ".json")
            .unwrap()
            .into_iter()
            .find(|n| n.starts_with("_verdicts-"))
            .expect("verdicts json");
        let verdicts =
            V::parse(&storage::read(&repo.0, &Path::new(dir).join(&verdicts_name), false).unwrap())
                .unwrap();
        assert_eq!(verdicts.get("base").text(), Some(base_sha.as_str()));
        let seat = &verdicts.get("seats").array()[0];
        assert_eq!(seat.get("verdict").text(), Some("timed_out"));
        assert_eq!(seat.get("timeout_secs").py(), "1");
        assert_eq!(seat.get("diff_bytes").py(), diff_bytes.to_string());
        let elapsed_secs: u64 = seat.get("elapsed_secs").py().parse().unwrap();
        assert!(
            elapsed_secs >= 1,
            "expected elapsed >= 1s, got {elapsed_secs}"
        );
        assert!(
            seat.get("detail")
                .text()
                .is_some_and(|d| d.contains("timed out after 1s") && !d.contains("interrupted")),
            "{seat:?}"
        );

        let inspected = run(&repo, &repo.0, "peer-review", &["verdicts", dir, "--json"]);
        assert_eq!(inspected.0, 2);
        let inspected_json = V::parse(&inspected.1).unwrap();
        assert_eq!(
            inspected_json.get("seats").array()[0].get("verdict").text(),
            Some("timed_out")
        );
        let inspected_text = run(&repo, &repo.0, "peer-review", &["verdicts", dir]);
        assert_eq!(inspected_text.0, 2);
        let text_out = String::from_utf8_lossy(&inspected_text.1);
        assert!(text_out.contains("codex: timed out after 1s"), "{text_out}");
        assert!(!text_out.contains("interrupted"), "{text_out}");
    });

    // #457 large-diff timeout hint + interrupted wording preservation + #458 pinned --base SHA.
    provider_isolated_with_env(name, "default-wall", &[], || {
        use oms_command_peer::storage;
        let repo = provider_repo();
        repo.write("lib.rs", "pub fn v() -> i32 { 1 }\n");
        repo.git(&["add", "lib.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "initial base",
        ]);
        let base_v1 = git_stdout(&repo, &["rev-parse", "HEAD"]);
        repo.git(&["update-ref", "refs/remotes/origin/main", &base_v1]);

        // Build a second commit on a side ref to simulate origin/main advancing during review.
        repo.git(&["checkout", "-qb", "other-main"]);
        repo.write("other.rs", "pub fn merged_pr() -> i32 { 99 }\n");
        repo.git(&["add", "other.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "merged elsewhere",
        ]);
        let base_v2 = git_stdout(&repo, &["rev-parse", "HEAD"]);

        repo.git(&["checkout", "-qb", "feature", &base_v1]);
        repo.write("lib.rs", "pub fn v() -> i32 { 2 }\n");
        repo.git(&["add", "lib.rs"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "feature change",
        ]);
        let feature_diff_bytes = Command::new("git")
            .arg("-C")
            .arg(&repo.0)
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                &base_v1,
                "--",
            ])
            .output()
            .unwrap()
            .stdout
            .len();
        assert!(feature_diff_bytes > 0);

        enum Mode {
            MoveBaseRef,
            AdvanceHead,
            InterruptedSeat,
            LargeDiffTimeout,
        }
        struct GateProbe<'a> {
            repo: &'a Repo,
            mode: Mode,
            base_v2: &'a str,
        }
        impl Executor for GateProbe<'_> {
            fn execute(
                &self,
                request: Invocation,
                stop: &AtomicBool,
            ) -> oms_command_peer::Result<Completed> {
                if request.argv[0] == "git" {
                    return Native.execute(request, stop);
                }
                if request.argv[0] == "bash" {
                    return Ok(Completed {
                        code: 0,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        complete: true,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::ZERO,
                    });
                }
                match self.mode {
                    Mode::MoveBaseRef => {
                        self.repo
                            .git(&["update-ref", "refs/remotes/origin/main", self.base_v2]);
                        Ok(Completed {
                            code: 0,
                            stdout: b"CONFIDENCE: 0.9\nGATE: pass\n".to_vec(),
                            stderr: Vec::new(),
                            complete: true,
                            cleanup_complete: Some(true),
                            owned_cleanup: None,
                            elapsed: Duration::from_secs(4),
                        })
                    }
                    Mode::AdvanceHead => {
                        self.repo.git(&[
                            "-c",
                            "user.name=Fixture",
                            "-c",
                            "user.email=fixture@example.invalid",
                            "commit",
                            "--allow-empty",
                            "-qm",
                            "head moved during review",
                        ]);
                        Ok(Completed {
                            code: 0,
                            stdout: b"CONFIDENCE: 0.9\nGATE: pass\n".to_vec(),
                            stderr: Vec::new(),
                            complete: true,
                            cleanup_complete: Some(true),
                            owned_cleanup: None,
                            elapsed: Duration::from_secs(2),
                        })
                    }
                    Mode::InterruptedSeat => Ok(Completed {
                        code: 130,
                        stdout: Vec::new(),
                        stderr: b"warning: child caught signal\nerror: interrupted\n".to_vec(),
                        complete: false,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::from_secs(3),
                    }),
                    Mode::LargeDiffTimeout => Ok(Completed {
                        code: 124,
                        stdout: Vec::new(),
                        stderr: b"error: interrupted\n".to_vec(),
                        complete: false,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::from_secs(1200),
                    }),
                }
            }
        }

        let run_case = |mode: Mode, dir: &str, stop: &AtomicBool| {
            let probe = GateProbe {
                repo: &repo,
                mode,
                base_v2: &base_v2,
            };
            oms_command_peer::run_with_executor(
                "peer-review",
                &args(&[
                    "--providers",
                    "codex",
                    "--model",
                    "fixture",
                    "--gate",
                    "--base",
                    "refs/remotes/origin/main",
                    "--verify",
                    "true",
                    "--prompt",
                    "verify pinned base and diagnostics",
                    "--no-task",
                    "--no-memory",
                    "--no-ml-context",
                    "--no-graph-context",
                    "--artifact-dir",
                    dir,
                ]),
                &repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                stop,
                &probe,
            )
        };

        // 1. Moving refs/remotes/origin/main during the review must NOT invalidate the gate.
        let moved = run_case(Mode::MoveBaseRef, ".oms/review-pinned-base", &STOP);
        let moved_out = String::from_utf8_lossy(&moved.1);
        assert_eq!(
            moved.0, 0,
            "moving base ref must not invalidate gate: {moved:?}"
        );
        assert!(!moved_out.contains("review scope changed"), "{moved_out}");
        assert!(
            moved_out.contains(&format!("base: {base_v1}")),
            "stdout must record resolved base SHA: {moved_out}"
        );
        assert!(
            moved_out.contains(&format!("elapsed 4s, diff {feature_diff_bytes} bytes")),
            "{moved_out}"
        );
        let verdicts_name = storage::names(&repo.0, Path::new(".oms/review-pinned-base"), ".json")
            .unwrap()
            .into_iter()
            .find(|n| n.starts_with("_verdicts-"))
            .expect("verdicts json");
        let verdicts = V::parse(
            &storage::read(
                &repo.0,
                &Path::new(".oms/review-pinned-base").join(&verdicts_name),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(verdicts.get("base").text(), Some(base_v1.as_str()));
        assert_eq!(
            verdicts.get("seats").array()[0].get("elapsed_secs").py(),
            "4"
        );
        assert_eq!(
            verdicts.get("seats").array()[0].get("diff_bytes").py(),
            feature_diff_bytes.to_string()
        );

        // Reset origin/main back to base_v1 for subsequent cases.
        repo.git(&["update-ref", "refs/remotes/origin/main", &base_v1]);

        // 2. An interrupted seat keeps the interrupted wording (not timed out).
        let interrupted_seat =
            run_case(Mode::InterruptedSeat, ".oms/review-interrupted-seat", &STOP);
        assert_eq!(interrupted_seat.0, 2, "{interrupted_seat:?}");
        let int_out = String::from_utf8_lossy(&interrupted_seat.1);
        assert!(int_out.contains("codex: interrupted"), "{int_out}");
        assert!(!int_out.contains("timed out"), "{int_out}");
        let int_verdicts_name =
            storage::names(&repo.0, Path::new(".oms/review-interrupted-seat"), ".json")
                .unwrap()
                .into_iter()
                .find(|n| n.starts_with("_verdicts-"))
                .expect("verdicts json");
        let int_verdicts = V::parse(
            &storage::read(
                &repo.0,
                &Path::new(".oms/review-interrupted-seat").join(&int_verdicts_name),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            int_verdicts.get("seats").array()[0].get("verdict").text(),
            Some("interrupted")
        );
        let inspected_int = run(
            &repo,
            &repo.0,
            "peer-review",
            &["verdicts", ".oms/review-interrupted-seat"],
        );
        assert_eq!(inspected_int.0, 2);
        let inspected_int_out = String::from_utf8_lossy(&inspected_int.1);
        assert!(
            inspected_int_out.contains("codex: interrupted"),
            "{inspected_int_out}"
        );
        assert!(
            !inspected_int_out.contains("timed out"),
            "{inspected_int_out}"
        );

        // Real cancellation via AtomicBool keeps interrupted wording.
        let cancelled_flag = AtomicBool::new(true);
        let cancelled = run_case(Mode::MoveBaseRef, ".oms/review-cancelled", &cancelled_flag);
        assert_eq!(cancelled.0, 130);
        let cancel_err = String::from_utf8_lossy(&cancelled.2);
        assert!(cancel_err.contains("interrupted"), "{cancel_err}");
        assert!(!cancel_err.contains("timed out"), "{cancel_err}");

        // 3. Large diff timeout at default 20m suggests raising OMS_PEER_TIMEOUT.
        let large_body = (0..400)
            .map(|i| {
                format!(
                    "pub fn generated_line_{i:04}() -> usize {{ {i} }} // padding-padding-padding\n"
                )
            })
            .collect::<String>();
        repo.write("lib.rs", &large_body);
        let large_diff_bytes = Command::new("git")
            .arg("-C")
            .arg(&repo.0)
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                &base_v1,
                "--",
            ])
            .output()
            .unwrap()
            .stdout
            .len();
        assert!(
            large_diff_bytes >= 16 * 1024,
            "large_diff_bytes={large_diff_bytes}"
        );
        let large_timeout = run_case(Mode::LargeDiffTimeout, ".oms/review-large-timeout", &STOP);
        assert_eq!(large_timeout.0, 2, "{large_timeout:?}");
        let large_out = String::from_utf8_lossy(&large_timeout.1);
        assert!(
            large_out.contains("codex: timed out after 20m")
                && large_out.contains("OMS_PEER_TIMEOUT")
                && large_out.contains(&format!("elapsed 1200s, diff {large_diff_bytes} bytes")),
            "{large_out}"
        );
        assert!(
            !large_out.to_lowercase().contains("interrupted"),
            "{large_out}"
        );

        // 4. Changing HEAD during the review still invalidates the gate.
        repo.write("lib.rs", "pub fn v() -> i32 { 2 }\n");
        let head_moved = run_case(Mode::AdvanceHead, ".oms/review-head-moved", &STOP);
        assert_eq!(head_moved.0, 2, "{head_moved:?}");
        let head_out = String::from_utf8_lossy(&head_moved.1);
        assert!(
            head_out.contains("review scope changed during provider calls; gate incomplete"),
            "{head_out}"
        );
    });
}
#[test]
fn unavailable_services_refuse() {
    use oms_runtime::Services;
    let services = oms_runtime::services::Unavailable;
    assert!(services.repository_view(Path::new(".")).is_err());
    assert!(
        services
            .task_status(Path::new("."), Path::new("."), b"")
            .is_err()
    );
    assert!(
        services
            .plan_status(Path::new("."), Path::new("."), b"")
            .is_err()
    );
}
#[test]
fn review_depth_preserves_risk_globs() {
    isolated("review_depth_preserves_risk_globs", || {
        let repo = Repo::new();
        let source = Repo::new();
        source.write("config/review-depth.json", br#"{"schema":1,"none_lines":10,"gate_lines":200,"gate_files":10,"council_lines":800,"council_files":30,"contract_paths":[]}"#);
        for path in [
            "hooks/pre-commit",
            "plugins/fixture/hooks.json",
            "scripts/lib/hook_state.py",
            "crates/oms-command-room/src/lib.rs",
            "hooks.json",
        ] {
            repo.write(path, "baseline\n");
        }
        repo.git(&["add", "."]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        let advice = || {
            V::parse(&success(run(
                &repo,
                &source.0,
                "peer-review",
                &["advise", "--json"],
            )))
            .unwrap()
        };
        assert_eq!(advice().get("depth").text(), Some("none"));
        assert!(matches!(advice().get("spec_stage"), V::Bool(false)));
        for (path, risk) in [
            ("hooks/pre-commit", true),
            ("plugins/fixture/hooks.json", true),
            ("scripts/lib/hook_state.py", true),
            ("crates/oms-command-room/src/lib.rs", false),
            ("hooks.json", false),
        ] {
            repo.write(path, "changed\n");
            let value = advice();
            assert_eq!(
                value.get("depth").text(),
                Some(if risk { "gate+security" } else { "none" }),
                "{path}"
            );
            assert_eq!(!value.get("risk_paths").array().is_empty(), risk, "{path}");
            assert_eq!(value.get("spec_stage").truth(), risk, "{path}");
            repo.write(path, "baseline\n");
        }
        // The spec stage precedes only the expensive depths; sizes are counted
        // as added plus removed lines against the thresholds above.
        for (count, depth, spec) in [
            (11, "single", false),
            (200, "gate+security", true),
            (800, "council", true),
        ] {
            repo.write("hooks.json", "changed\n".repeat(count));
            let value = advice();
            assert_eq!(value.get("depth").text(), Some(depth), "{count}");
            assert_eq!(value.get("spec_stage").truth(), spec, "{count}");
        }
    });
}

// Linux exposes bytes actually read, so the hot-path check needs no timing
// threshold. Other supported hosts still exercise the same output contracts.
fn read_bytes() -> Option<u64> {
    fs::read_to_string("/proc/thread-self/io")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("rchar: ")?.trim().parse().ok())
}

#[test]
fn thread_hot_paths_read_each_log_once_and_keep_stale_contracts() {
    isolated(
        "thread_hot_paths_read_each_log_once_and_keep_stale_contracts",
        || {
            use oms_command_peer::{obj, providers::answer_quality, storage};
            use std::os::unix::fs::MetadataExt;
            let repo = Repo::new();
            let source = Repo::new();
            let row = |role: &str, body: &str, provider: &str| {
                obj([
                    ("thread", "history".into()),
                    ("role", role.into()),
                    ("text", body.into()),
                    ("ts", "2000-01-01T00:00:00Z".into()),
                    ("provider", provider.into()),
                    ("quality", "ok".into()),
                ])
            };
            let mut rows = vec![row("question", "first question", "codex")];
            rows.push(row("topic", "  chosen\n topic  ", ""));
            for i in 0..64 {
                rows.push(row(
                    if i == 0 { "question" } else { "answer" },
                    &"x".repeat(4096),
                    if i % 2 == 0 { "claude" } else { "codex" },
                ));
            }
            let mut bytes = Vec::new();
            for row in &rows {
                bytes.extend(row.json(false).unwrap());
                bytes.push(b'\n');
            }
            // Calibrate host /proc/*/stat scan overhead on an empty thread listing
            // so hosts with hundreds of processes do not masquerade as extra log reads.
            let base = {
                let b0 = read_bytes();
                let _ = run(&repo, &source.0, "thread", &["list", "--json"]);
                b0.zip(read_bytes())
                    .map_or(0, |(start, end)| end.saturating_sub(start))
            };
            repo.write(".oms/threads/history.jsonl", &bytes);
            let minted = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            repo.write(".oms/threads/CURRENT", format!("history {minted}\n"));
            for action in ["list", "stats", "show"] {
                let before = read_bytes();
                let output = success(run(&repo, &source.0, "thread", &[action, "--json"]));
                if let (Some(before), Some(after)) = (before, read_bytes()) {
                    assert!(
                        after - before < bytes.len() as u64 + base + 32768,
                        "{action} read {} bytes for a {} byte log (baseline {base})",
                        after - before,
                        bytes.len()
                    );
                }
                let value = V::parse(&output).unwrap();
                match action {
                    "list" => {
                        let entry = &value.get("threads").array()[0];
                        assert_eq!(entry.get("topic").text(), Some("chosen topic"));
                        assert_eq!(
                            entry.get("providers"),
                            &V::Arr(vec!["codex".into(), "claude".into()])
                        );
                        assert_eq!(entry.get("staleness").text(), Some("current"));
                    }
                    "stats" => {
                        for key in ["threads", "open", "second_question", "multi_provider"] {
                            assert_eq!(value.get("stats").get(key).py(), "1");
                        }
                        assert_eq!(value.get("stats").get("answers_ok").py(), "63");
                    }
                    "show" => assert_eq!(value, V::Arr(rows.clone())),
                    _ => unreachable!(),
                }
            }
            fs::remove_file(repo.0.join(".oms/threads/CURRENT")).unwrap();
            for ttl in ["0", "9223372036854775807"] {
                let output = V::parse(&success(run(
                    &repo,
                    &source.0,
                    "thread",
                    &["stats", "--json", "--stale-ttl", ttl],
                )))
                .unwrap();
                assert_eq!(output.get("stale_ttl_seconds").py(), ttl);
                assert_eq!(
                    output.get("stats").get("stale_open").py(),
                    if ttl == "0" { "1" } else { "0" }
                );
            }
            for ttl in ["", "-1", "bad", "9223372036854775808"] {
                for action in ["list", "stats", "close"] {
                    let mut argv = vec![action, "--stale-ttl", ttl];
                    if action == "close" {
                        argv.push("--stale");
                    }
                    assert_eq!(run(&repo, &source.0, "thread", &argv).0, 2, "{argv:?}");
                }
            }
            assert_eq!(
                run(&repo, &source.0, "thread", &["close", "--stale-ttl", "0"]).0,
                2
            );
            let room = row("note", "room", "");
            let mut room = room;
            room.set("thread", "room-0123456789ab");
            repo.write(
                ".oms/threads/room-0123456789ab.jsonl",
                format!(
                    "{}\n",
                    String::from_utf8(room.json(false).unwrap()).unwrap()
                ),
            );
            let mut event = row("note", "room event", "");
            event.set("thread", "event-room");
            event.set("room_event", obj([("kind", "created".into())]));
            repo.write(
                ".oms/threads/event-room.jsonl",
                format!(
                    "{}\n",
                    String::from_utf8(event.json(false).unwrap()).unwrap()
                ),
            );
            success(run(
                &repo,
                &source.0,
                "thread",
                &["close", "--stale", "--stale-ttl", "0", "--apply"],
            ));
            for name in ["room-0123456789ab", "event-room"] {
                let value = V::parse(&success(run(
                    &repo,
                    &source.0,
                    "thread",
                    &["show", "--id", name, "--json"],
                )))
                .unwrap();
                assert_eq!(value.array().len(), 1, "room remains open");
            }
            let value = V::parse(&success(run(
                &repo,
                &source.0,
                "thread",
                &["show", "--id", "history", "--json"],
            )))
            .unwrap();
            assert_eq!(
                value.array().last().unwrap().get("role").text(),
                Some("closed")
            );

            let records = vec![
                obj([("id", "b".into()), ("n", 1usize.into())]),
                obj([("id", "a".into())]),
                obj([("id", "b".into()), ("n", 2usize.into())]),
            ];
            assert_eq!(
                storage::latest(&records).unwrap(),
                vec![records[2].clone(), records[1].clone()]
            );
            assert!(storage::latest(&[]).unwrap().is_empty());
            assert!(storage::latest(&[obj([])]).is_err());
            for (raw, expected) in [
                ("", "empty"),
                ("Answer:\nVerification:\r\n", "empty"),
                ("a substantive response", "ok"),
                (
                    "stop-reason: provider=codex reason=refusal subtype=test is_error=0",
                    "blocked",
                ),
                (
                    "[agy] print timeout after 5s with turn in progress; returning partial output",
                    "truncated",
                ),
                (
                    "stop-reason: provider=codex reason=stream_truncated subtype=test is_error=0",
                    "truncated",
                ),
                (
                    "stop-reason: provider=codex reason=refusal subtype=test is_error=0\n## Output\nanswer",
                    "ok",
                ),
                (
                    "## Output\r\nstop-reason: provider=codex reason=refusal subtype=test is_error=1",
                    "blocked",
                ),
                ("served model\nvalue\nconfigured model\nvalue", "empty"),
                ("jetski: no output produced", "blocked"),
                ("jetski: no output produced\nanswer", "ok"),
            ] {
                assert_eq!(answer_quality(raw), expected);
                assert_eq!(answer_quality(raw), expected, "cached expression parity");
            }
            repo.write(
                "review/_findings-first.jsonl",
                concat!(
                    "{\"kind\":\"review-finding\",\"id\":\"a\",\"run\":\"r1\"}\n",
                    "{\"kind\":\"review-finding\",\"id\":\"b\",\"run\":\"r1\"}\n",
                    "{\"kind\":\"review-finding\",\"id\":\"b\",\"run\":\"r2\"}\n",
                ),
            );
            repo.write("review/_finding-events.jsonl", concat!(
                "{\"kind\":\"review-finding-event\",\"id\":\"a\",\"state\":\"open\"}\n",
                "{\"kind\":\"review-finding-confirmation\",\"id\":\"a\",\"result\":\"old\"}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"b\",\"state\":\"fixed\",\"runs\":[\"r1\"]}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"a\",\"state\":\"fixed\",\"runs\":[\"r1\"]}\n",
                "{\"kind\":\"review-finding-confirmation\",\"id\":\"a\",\"result\":\"new\"}\n",
                "{\"kind\":\"review-finding-event\",\"id\":\"absent\",\"state\":\"fixed\"}\n",
            ));
            let findings = V::parse(&success(run(
                &repo,
                &source.0,
                "peer-review",
                &["findings", "list", "--artifact-dir", "review", "--json"],
            )))
            .unwrap();
            assert_eq!(findings.array().len(), 2);
            assert_eq!(findings.array()[0].get("status").text(), Some("fixed"));
            assert_eq!(findings.array()[0].get("history").array().len(), 2);
            assert_eq!(
                findings.array()[0].get("confirmation").get("result").text(),
                Some("new")
            );
            assert_eq!(findings.array()[1].get("status").text(), Some("open"));
            storage::write(
                &repo.0,
                Path::new("same.json"),
                b"{}\n".to_vec(),
                true,
                &STOP,
            )
            .unwrap();
            let first = fs::metadata(repo.0.join("same.json")).unwrap();
            storage::write(
                &repo.0,
                Path::new("same.json"),
                b"{}\n".to_vec(),
                true,
                &STOP,
            )
            .unwrap();
            let second = fs::metadata(repo.0.join("same.json")).unwrap();
            assert_eq!(
                (first.ino(), first.mtime(), first.mtime_nsec()),
                (second.ino(), second.mtime(), second.mtime_nsec())
            );
        },
    );
}

#[test]
fn delegate_warm_cargo_slot_cache_and_opt_out() {
    isolated("delegate_warm_cargo_slot_cache_and_opt_out", || {
        use oms_command_peer::process::{Completed, Executor, Invocation};
        use std::sync::Mutex;

        struct EnvRecorder(Mutex<Vec<(String, Option<String>)>>);
        impl Executor for EnvRecorder {
            fn execute(
                &self,
                request: Invocation,
                interrupted: &AtomicBool,
            ) -> oms_command_peer::Result<Completed> {
                let prog = request
                    .argv
                    .first()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned();
                if !prog.ends_with("git") {
                    let found = request
                        .environment
                        .iter()
                        .find(|(k, _)| k == "CARGO_TARGET_DIR")
                        .map(|(_, v)| v.to_string_lossy().into_owned());
                    self.0.lock().unwrap().push((prog, found));
                    return Ok(Completed {
                        code: 0,
                        stdout: b"ok\n".to_vec(),
                        stderr: Vec::new(),
                        complete: true,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: std::time::Duration::ZERO,
                    });
                }
                oms_command_peer::process::Native.execute(request, interrupted)
            }
        }

        let repo = Repo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n",
        );
        repo.git(&["add", "Cargo.toml"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "cargo repo",
        ]);
        let fake_codex = repo.0.join(".fake-bin/codex");
        fs::create_dir_all(fake_codex.parent().unwrap()).unwrap();
        fs::write(&fake_codex, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap();
        let xdg_cache = home.join(".cache");
        let path_val = format!(
            "{}:{}",
            fake_codex.parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        if std::env::var_os("OMS_SLOT_INNER").is_none() {
            for mode in ["slot", "optout"] {
                let mut cmd = Command::new(std::env::current_exe().unwrap());
                cmd.args([
                    "--exact",
                    "delegate_warm_cargo_slot_cache_and_opt_out",
                    "--nocapture",
                ])
                .env(
                    "CONTRACT_CHILD",
                    "delegate_warm_cargo_slot_cache_and_opt_out",
                )
                .env("OMS_SLOT_INNER", mode)
                .env("XDG_CACHE_HOME", &xdg_cache)
                .env("PATH", &path_val)
                .env_remove("CARGO_TARGET_DIR");
                if mode == "optout" {
                    cmd.env("OMS_DELEGATE_ISOLATED_BUILD", "1");
                } else {
                    cmd.env_remove("OMS_DELEGATE_ISOLATED_BUILD");
                }
                let out = cmd.output().unwrap();
                assert!(
                    out.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            return;
        }
        let mode = std::env::var("OMS_SLOT_INNER").unwrap();
        let recorder = EnvRecorder(Mutex::new(Vec::new()));
        let values = args(&[
            "--repo",
            repo.0.to_str().unwrap(),
            "--to",
            "codex",
            "--model",
            "fixture",
            "--verify",
            "cargo test",
            "--allow-no-change",
            "Build check",
        ]);
        let stop = AtomicBool::new(false);
        let out = oms_command_peer::run_with_executor(
            "peer-delegate",
            &values,
            &repo.0,
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &stop,
            &recorder,
        );
        assert_eq!(
            out.0,
            0,
            "delegate failed: {}",
            String::from_utf8_lossy(&out.2)
        );
        let calls = recorder.0.lock().unwrap();
        assert!(!calls.is_empty());
        if mode == "optout" {
            assert!(
                calls[0].1.is_none(),
                "OMS_DELEGATE_ISOLATED_BUILD=1 must omit CARGO_TARGET_DIR"
            );
        } else {
            let first_dir = calls[0]
                .1
                .as_ref()
                .expect("slot-0 CARGO_TARGET_DIR assigned");
            assert!(
                first_dir.contains("cargo-slots/") && first_dir.ends_with("/slot-0/target"),
                "unexpected slot path: {first_dir}"
            );
        }
    });
}

#[test]
fn delegate_interactive_ndjson_runs_multiple_turns_in_one_worktree() {
    isolated(
        "delegate_interactive_ndjson_runs_multiple_turns_in_one_worktree",
        || {
            let repo = Repo::new();
            repo.write("README.md", "interactive base\n");
            repo.git(&["add", "README.md"]);
            repo.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "init",
            ]);
            let fake_claude = repo.0.join(".fake-bin/claude");
            fs::create_dir_all(fake_claude.parent().unwrap()).unwrap();
            fs::write(
                &fake_claude,
                "#!/bin/sh\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"turn-ack\"}\\n'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let path_val = format!(
                "{}:{}",
                fake_claude.parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            );
            if std::env::var_os("OMS_INTERACTIVE_INNER").is_none() {
                use std::io::Write;
                let mut cmd = Command::new(std::env::current_exe().unwrap());
                cmd.args([
                    "--exact",
                    "delegate_interactive_ndjson_runs_multiple_turns_in_one_worktree",
                    "--nocapture",
                ])
                .env(
                    "CONTRACT_CHILD",
                    "delegate_interactive_ndjson_runs_multiple_turns_in_one_worktree",
                )
                .env("OMS_INTERACTIVE_INNER", "1")
                .env("PATH", &path_val)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
                let mut child = cmd.spawn().unwrap();
                // Keep the pipe open after finish: the child must exit without EOF.
                let mut stdin = child.stdin.take().unwrap();
                stdin
                    .write_all(
                        b"{\"prompt\":\"first turn\"}\n{\"prompt\":\"second turn\"}\n{\"action\":\"finish\"}\n",
                    )
                    .unwrap();
                stdin.flush().unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(120);
                while child.try_wait().unwrap().is_none() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "child waited for stdin EOF after finish"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                drop(stdin);
                let out = child.wait_with_output().unwrap();
                assert!(
                    out.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                return;
            }
            let values = args(&[
                "--repo",
                repo.0.to_str().unwrap(),
                "--to",
                "claude",
                "--model",
                "fixture",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--no-verify",
                "--allow-no-change",
                "--interactive",
                "Interactive base",
            ]);
            oms_command_peer::buffer_interactive_stdout(true);
            let stop = AtomicBool::new(false);
            let out = oms_command_peer::run_with_executor(
                "peer-delegate",
                &values,
                &repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                &stop,
                &Native,
            );
            let stdout = String::from_utf8_lossy(&out.1);
            assert_eq!(
                out.0,
                0,
                "interactive failed: {stdout}\n{}",
                String::from_utf8_lossy(&out.2)
            );
            assert!(stdout.contains("\"turn\":1"), "{stdout}");
            assert!(stdout.contains("\"turn\":2"), "{stdout}");
            let md = fs::read_dir(repo.0.join(".oms/artifacts/delegate"))
                .unwrap()
                .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(md.contains("## Turn 1") && md.contains("## Turn 2"), "{md}");
        },
    );
}

#[test]
fn delegate_context_manifest_binds_snapshot_bundle_across_repair() {
    provider_isolated(
        "delegate_context_manifest_binds_snapshot_bundle_across_repair",
        "private",
        || {
            struct Repair(ProviderProbe);
            impl Executor for Repair {
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    if request.argv[0] != "codex" {
                        return Native.execute(request, stop);
                    }
                    let round = self.0.calls.lock().unwrap().len() + 1;
                    fs::write(request.cwd.join("tracked.txt"), format!("round {round}\n")).unwrap();
                    self.0.execute(request, stop)
                }
            }
            let source = Repo::new();
            for entry in fs::read_dir(Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../config"
            )))
            .unwrap()
            {
                let path = entry.unwrap().path();
                if path.is_file() {
                    source.write(
                        &format!("config/{}", path.file_name().unwrap().to_str().unwrap()),
                        fs::read(&path).unwrap(),
                    );
                }
            }
            let repo = provider_repo();
            repo.write(".gitignore", ".oms/\n");
            repo.write("PROJECT.md", "# Project\ncommitted contract marker\n");
            repo.write("AGENTS.md", "문맥 🙂\n".repeat(12000));
            repo.write("src/main.rs", "fn main() {}\n");
            repo.write("src/AGENTS.md", "Target contract marker\n");
            std::os::unix::fs::symlink("src/main.rs", repo.0.join("alias.rs")).unwrap();
            std::os::unix::fs::symlink("src", repo.0.join("linked")).unwrap();
            repo.git(&[
                "add",
                ".gitignore",
                "PROJECT.md",
                "AGENTS.md",
                "src",
                "alias.rs",
                "linked",
            ]);
            repo.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "context snapshot",
            ]);
            repo.write("PROJECT.md", "dirty parent contract marker\n");
            repo.write(
                ".oms/task/current.md",
                "parent task must not enter snapshot bundle\n",
            );
            repo.write("pack.json", format!("{{\"pack_digest\":\"{}\",\"files\":[\"src/main.rs\",\"stale.rs\",\"alias.rs\",\"linked/main.rs\"]}}", "a".repeat(64)));
            let values = args(&[
                "--repo",
                repo.0.to_str().unwrap(),
                "--to",
                "codex",
                "--model",
                "fixture",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--context-manifest",
                "--context-pack",
                "pack.json",
                "--scope",
                "tracked.txt",
                "--repair",
                "1",
                "--verify",
                "grep -q 'round 2' tracked.txt",
                "Change tracked.txt",
            ]);
            let probe = Repair(ProviderProbe::default());
            let output = oms_command_peer::run_with_executor(
                "peer-delegate",
                &values,
                &repo.0,
                &source.0,
                &STOP,
                &probe,
            );
            assert_eq!(
                output.0,
                0,
                "{}{}",
                String::from_utf8_lossy(&output.1),
                String::from_utf8_lossy(&output.2)
            );
            let directory = repo.0.join(".oms/runtime/context");
            let manifests: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|s| s == "json"))
                .collect();
            assert_eq!(manifests.len(), 1, "compile only once");
            let raw = fs::read(&manifests[0]).unwrap();
            let manifest = V::parse(&raw).unwrap();
            oms_context::inspect_manifest(&raw, 100).unwrap();
            let bundle = fs::read(manifests[0].with_extension("txt")).unwrap();
            let text = std::str::from_utf8(&bundle).unwrap();
            assert!(text.contains("committed contract marker"));
            assert!(text.contains("Target contract marker"));
            assert!(text.contains("fn main()"));
            assert!(!text.contains("dirty parent contract marker"));
            assert!(!text.contains("parent task must not enter"));
            assert!(!manifest.get("sufficient").truth());
            assert!(
                manifest
                    .get("targets")
                    .array()
                    .iter()
                    .all(|v| v.text() == Some("src/main.rs"))
            );
            let calls = probe.0.calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            for call in calls.iter() {
                assert!(call.prompt.windows(bundle.len()).any(|w| w == bundle));
                assert!(
                    std::str::from_utf8(&call.prompt)
                        .unwrap()
                        .contains("this bounded bundle is partial")
                );
            }
            let recorded: Vec<_> = fs::read_dir(directory.join("calls"))
                .unwrap()
                .map(|e| V::parse(&fs::read(e.unwrap().path()).unwrap()).unwrap())
                .collect();
            assert_eq!(recorded.len(), 2);
            for call in calls.iter() {
                let hash = oms_command_peer::storage::hash(&call.prompt);
                let row = recorded
                    .iter()
                    .find(|row| row.get("attribution").get("request_sha256").text() == Some(&hash))
                    .unwrap();
                let attribution = row.get("attribution");
                assert_eq!(
                    attribution.get("context_manifest_sha256").text(),
                    Some(oms_command_peer::storage::hash(&raw).as_str())
                );
                assert_eq!(
                    attribution.get("phase_policy_sha256"),
                    manifest.get("phase_rules_sha256")
                );
                assert_eq!(row.get("dispatch").text(), Some("executor-returned"));
                assert_eq!(row.get("provider_delivery"), &V::Null);
                for span in attribution.get("spans").array() {
                    let integer = |key| match span.get(key) {
                        V::Num(s) => s.parse::<usize>().unwrap(),
                        _ => panic!("span byte offset must be an integer"),
                    };
                    let (start, end) = (integer("start"), integer("end"));
                    assert_eq!(
                        span.get("sha256").text(),
                        Some(oms_command_peer::storage::hash(&call.prompt[start..end]).as_str())
                    );
                    assert_eq!(
                        span.get("prefix_sha256").text(),
                        Some(oms_command_peer::storage::hash(&call.prompt[..end]).as_str())
                    );
                }
            }
            let index = fs::read_to_string(repo.0.join(".oms/artifacts/index.jsonl")).unwrap();
            let row = index
                .lines()
                .map(|s| V::parse(s.as_bytes()).unwrap())
                .find(|v| v.get("kind").text() == Some("delegate"))
                .unwrap();
            assert_eq!(
                row.get("context_manifest_digest").text(),
                Some(oms_command_peer::storage::hash(&raw).as_str())
            );
            assert_eq!(
                row.get("context_bundle_sha256"),
                manifest.get("bundle_sha256")
            );
            assert_eq!(
                row.get("context_selected_bytes"),
                manifest.get("selected_bytes")
            );
            assert_eq!(row.get("context_debt"), manifest.get("context_debt"));
            assert!(calls.iter().all(|call| !call.request.cwd.exists()));
            let prepare = |case: &Repo, extra: &[&str]| {
                let probe = ProviderProbe::default();
                let mut values = args(&[
                    "--repo",
                    case.0.to_str().unwrap(),
                    "--to",
                    "codex",
                    "--model",
                    "fixture",
                    "--no-task",
                    "--no-memory",
                    "--no-ml-context",
                    "--context-manifest",
                    "--verify",
                    "true",
                ]);
                values.extend(args(extra));
                values.push("Change tracked.txt".into());
                let result = oms_command_peer::run_with_executor(
                    "peer-delegate",
                    &values,
                    &case.0,
                    &source.0,
                    &STOP,
                    &probe,
                );
                assert!(
                    probe.calls.lock().unwrap().is_empty(),
                    "preparation must not start a worker"
                );
                assert!(!case.0.join(".oms/lifecycle/events.jsonl").exists());
                let worktrees = Command::new("git")
                    .arg("-C")
                    .arg(&case.0)
                    .args(["worktree", "list", "--porcelain"])
                    .output()
                    .unwrap();
                let retained =
                    String::from_utf8_lossy(&result.2).contains("context snapshot retained: ");
                assert_eq!(
                    String::from_utf8_lossy(&worktrees.stdout)
                        .matches("worktree ")
                        .count(),
                    if retained { 2 } else { 1 }
                );
                result
            };
            let rules = fs::read(source.0.join("config/context-phases.json")).unwrap();
            source.write("config/context-phases.json", "{\"schema\":9}");
            let dry = provider_repo();
            assert_eq!(prepare(&dry, &["--dry-run"]).0, 0);
            assert!(!dry.0.join(".oms/runtime/context").exists());
            let invalid = provider_repo();
            assert_eq!(prepare(&invalid, &[]).0, 2);
            source.write("config/context-phases.json", rules);
            let oversized = provider_repo();
            oversized.write("large.rs", vec![b'a'; oms_runtime::storage::MAX_STATE + 1]);
            oversized.git(&["add", "large.rs"]);
            oversized.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "oversized context",
            ]);
            oversized.write(
                "pack.json",
                format!(
                    "{{\"pack_digest\":\"{}\",\"files\":[\"large.rs\"]}}",
                    "d".repeat(64)
                ),
            );
            assert_eq!(prepare(&oversized, &["--context-pack", "pack.json"]).0, 2);
            let linked = provider_repo();
            let outside = Repo::new();
            fs::create_dir_all(linked.0.join(".oms/runtime")).unwrap();
            std::os::unix::fs::symlink(&outside.0, linked.0.join(".oms/runtime/context")).unwrap();
            let before = fs::read_dir(&outside.0).unwrap().count();
            assert_eq!(prepare(&linked, &[]).0, 2);
            assert_eq!(fs::read_dir(&outside.0).unwrap().count(), before);
            let private_pack = provider_repo();
            private_pack.write(
                "pack.json",
                format!(
                    "{{\"pack_digest\":\"{}\",\"note\":\"{}{}\"}}",
                    "c".repeat(64),
                    "sk-",
                    "A".repeat(48)
                ),
            );
            assert_eq!(
                prepare(&private_pack, &["--context-pack", "pack.json"]).0,
                3
            );
            let missing_parser = provider_repo();
            missing_parser.write("target.py", "def target(): pass\n");
            missing_parser.git(&["add", "target.py"]);
            missing_parser.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "python context",
            ]);
            missing_parser.write(
                "pack.json",
                format!(
                    "{{\"pack_digest\":\"{}\",\"files\":[\"target.py\"]}}",
                    "b".repeat(64)
                ),
            );
            assert_eq!(
                prepare(&missing_parser, &["--context-pack", "pack.json"]).0,
                2
            );
            struct CleanupRace {
                snapshot: Mutex<Option<PathBuf>>,
                statuses: std::sync::atomic::AtomicUsize,
                removals: std::sync::atomic::AtomicUsize,
            }
            impl Executor for CleanupRace {
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    assert_eq!(
                        request.argv[0], "git",
                        "compile failure must not start a provider"
                    );
                    let added = request
                        .argv
                        .windows(2)
                        .any(|p| p[0] == "worktree" && p[1] == "add");
                    let removing = request
                        .argv
                        .windows(2)
                        .any(|p| p[0] == "worktree" && p[1] == "remove");
                    let pristine = request.argv.iter().any(|arg| arg == "--ignored=matching");
                    let snapshot =
                        added.then(|| PathBuf::from(&request.argv[request.argv.len() - 2]));
                    let result = Native.execute(request, stop)?;
                    if let Some(path) = snapshot {
                        *self.snapshot.lock().unwrap() = Some(path);
                    }
                    if pristine && self.statuses.fetch_add(1, Ordering::Relaxed) == 1 {
                        assert!(
                            result.stdout.is_empty(),
                            "inject only after status observed a clean snapshot"
                        );
                        let snapshot = self.snapshot.lock().unwrap().clone().unwrap();
                        fs::create_dir(snapshot.join(".oms")).unwrap();
                        fs::write(
                            snapshot.join(".oms/late-marker.txt"),
                            "after clean status\n",
                        )
                        .unwrap();
                    }
                    if removing {
                        self.removals.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(result)
                }
            }
            let raced = provider_repo();
            raced.write(".gitignore", ".oms/\n");
            raced.git(&["add", ".gitignore"]);
            raced.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "ignored race state",
            ]);
            let values = args(&[
                "--repo",
                raced.0.to_str().unwrap(),
                "--to",
                "codex",
                "--model",
                "fixture",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--context-manifest",
                "--verify",
                "true",
                "Change tracked.txt",
            ]);
            let valid_rules = fs::read(source.0.join("config/context-phases.json")).unwrap();
            source.write("config/context-phases.json", "{\"schema\":9}");
            let race = CleanupRace {
                snapshot: Mutex::new(None),
                statuses: std::sync::atomic::AtomicUsize::new(0),
                removals: std::sync::atomic::AtomicUsize::new(0),
            };
            let failed = oms_command_peer::run_with_executor(
                "peer-delegate",
                &values,
                &raced.0,
                &source.0,
                &STOP,
                &race,
            );
            source.write("config/context-phases.json", valid_rules);
            assert_eq!(failed.0, 2);
            let snapshot = race.snapshot.lock().unwrap().clone().unwrap();
            assert!(
                snapshot.exists(),
                "snapshot and late marker lost: {} status checks, {} removal(s)",
                race.statuses.load(Ordering::Relaxed),
                race.removals.load(Ordering::Relaxed)
            );
            assert_eq!(
                race.removals.load(Ordering::Relaxed),
                0,
                "compile failure must retain its snapshot"
            );
            assert_eq!(
                race.statuses.load(Ordering::Relaxed),
                1,
                "failure must not enter a status/remove cleanup window"
            );
            assert!(String::from_utf8_lossy(&failed.2).contains(&format!(
                "context snapshot retained: {}",
                snapshot.display()
            )));
            assert_eq!(
                fs::read_to_string(snapshot.join("tracked.txt")).unwrap(),
                "baseline\n"
            );
            struct ChangedSnapshot(Mutex<Option<PathBuf>>);
            impl Executor for ChangedSnapshot {
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    assert_eq!(
                        request.argv[0], "git",
                        "changed snapshot must not start a provider"
                    );
                    let added = request
                        .argv
                        .windows(2)
                        .any(|pair| pair[0] == "worktree" && pair[1] == "add");
                    let snapshot =
                        added.then(|| PathBuf::from(&request.argv[request.argv.len() - 2]));
                    let result = Native.execute(request, stop)?;
                    if let Some(path) = snapshot {
                        fs::create_dir(path.join(".oms")).unwrap();
                        fs::write(path.join(".oms/preserve.txt"), "external ignored data\n")
                            .unwrap();
                        *self.0.lock().unwrap() = Some(path);
                    }
                    Ok(result)
                }
            }
            let changed = provider_repo();
            changed.write(".gitignore", ".oms/\n");
            changed.git(&["add", ".gitignore"]);
            changed.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "ignored state",
            ]);
            let values = args(&[
                "--repo",
                changed.0.to_str().unwrap(),
                "--to",
                "codex",
                "--model",
                "fixture",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--context-manifest",
                "--verify",
                "true",
                "Change tracked.txt",
            ]);
            let probe = ChangedSnapshot(Mutex::new(None));
            let result = oms_command_peer::run_with_executor(
                "peer-delegate",
                &values,
                &changed.0,
                &source.0,
                &STOP,
                &probe,
            );
            assert_eq!(result.0, 2);
            assert!(String::from_utf8_lossy(&result.2).contains("preserved for inspection"));
            let snapshot = probe.0.lock().unwrap().take().unwrap();
            assert_eq!(
                fs::read_to_string(snapshot.join(".oms/preserve.txt")).unwrap(),
                "external ignored data\n"
            );
        },
    );
}

#[test]
fn panel_dispatch_delegate_records_parent_refs_and_index_digests() {
    isolated(
        "panel_dispatch_delegate_records_parent_refs_and_index_digests",
        || {
            let repo = Repo::new();
            repo.write("README.md", "panel base\n");
            repo.git(&["add", "README.md"]);
            repo.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "init",
            ]);
            let fake_claude = repo.0.join(".fake-bin/claude");
            fs::create_dir_all(fake_claude.parent().unwrap()).unwrap();
            fs::write(
                &fake_claude,
                "#!/bin/sh\ncase \"$*\" in *'--permission-mode plan'*) ;; *) printf 'changed\\n' >> README.md;; esac\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}\\n'\n",
            )
            .unwrap();
            fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
            let path_val = format!(
                "{}:{}",
                fake_claude.parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            );
            if std::env::var_os("OMS_PANEL_DISPATCH_INNER").is_none() {
                for mode in ["populated", "defaults", "outside"] {
                    let mut cmd = Command::new(std::env::current_exe().unwrap());
                    clean_env_child(&mut cmd)
                        .args([
                            "--exact",
                            "panel_dispatch_delegate_records_parent_refs_and_index_digests",
                            "--nocapture",
                        ])
                        .env(
                            CHILD_ENV,
                            "panel_dispatch_delegate_records_parent_refs_and_index_digests",
                        )
                        .env("OMS_PANEL_DISPATCH_INNER", mode)
                        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
                        .env("PATH", &path_val)
                        .env(
                            "OMS_PANEL_DISPATCH",
                            if mode == "outside" { "0" } else { "1" },
                        )
                        .env("OMS_TASK_ID", "ambient-task")
                        .env("OMS_PANEL_MAIN_ATTEMPT", "att_main1234")
                        .env("OMS_PANEL_ROLE", "worker")
                        .env("OMS_PANEL_STRATEGY", "builder")
                        .env("OMS_PANEL_OWNER", "antigravity")
                        .env("OMS_PANEL_PURPOSE", "implement")
                        .env("OMS_PANEL_WORKLOAD", "routine")
                        .env("OMS_PANEL_ACCESS", "write")
                        .env("OMS_PANEL_LABEL", "builder test")
                        .env("OMS_PANEL_ALIAS", " builder-alias ")
                        .env("OMS_ROOM_ID", "room-1")
                        .env("OMS_ROOM_PARTICIPANT", "call-p1")
                        .env("OMS_PLAN_LEASE_ID", " lease-1 ")
                        .env("OMS_PANEL_CONTINUES", "att_cafe")
                        .env("OMS_PANEL_MODEL", "ambient-model")
                        .env("OMS_PANEL_EFFORT", "low");
                    if mode == "defaults" {
                        for name in [
                            "OMS_PANEL_ROLE",
                            "OMS_PANEL_STRATEGY",
                            "OMS_PANEL_OWNER",
                            "OMS_PANEL_PURPOSE",
                            "OMS_PANEL_WORKLOAD",
                            "OMS_PANEL_ACCESS",
                            "OMS_PANEL_LABEL",
                            "OMS_PANEL_ALIAS",
                            "OMS_ROOM_PARTICIPANT",
                            "OMS_PLAN_LEASE_ID",
                        ] {
                            cmd.env(name, " ");
                        }
                        cmd.env("OMS_PANEL_CONTINUES", "att_BAD");
                    }
                    let out = cmd.output().unwrap();
                    assert!(
                        out.status.success(),
                        "{mode}: {}\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                return;
            }
            let mode = std::env::var("OMS_PANEL_DISPATCH_INNER").unwrap();
            let populated = mode == "populated";
            let panel = mode != "outside";
            let values = args(&[
                "--repo",
                repo.0.to_str().unwrap(),
                "--to",
                "claude",
                "--model",
                "fixture",
                "--reasoning-effort",
                "high",
                "--task-id",
                "t1",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "--verify",
                "true",
                "Panel dispatch change",
            ]);
            let stop = AtomicBool::new(false);
            let out = run_with_source(
                "peer-delegate",
                &values,
                &repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                &stop,
            );
            assert_eq!(
                out.0,
                0,
                "panel delegate failed: {}\n{}",
                String::from_utf8_lossy(&out.1),
                String::from_utf8_lossy(&out.2)
            );
            let events_raw =
                fs::read_to_string(repo.0.join(".oms/lifecycle/events.jsonl")).unwrap();
            let created = events_raw
                .lines()
                .map(|l| V::parse(l.as_bytes()).unwrap())
                .find(|v| v.get("event_type").text() == Some("attempt.created"))
                .expect("attempt.created event");
            assert_eq!(
                created.get("parent_attempt_id").text(),
                Some("att_main1234")
            );
            assert_eq!(created.get("task_id").text(), Some("t1"));
            let refs = created.get("refs");
            assert_eq!(refs.get("panel_model").text(), Some("fixture"));
            assert_eq!(
                refs.get("panel_role").text(),
                Some(if panel { "worker" } else { "delegate" })
            );
            for (key, expected) in [
                ("panel_strategy", populated.then_some("builder")),
                ("panel_owner", populated.then_some("antigravity")),
                ("panel_purpose", panel.then_some("implement")),
                ("panel_workload", panel.then_some("routine")),
                ("panel_access", panel.then_some("write")),
                ("panel_effort", panel.then_some("high")),
                (
                    "panel_label",
                    Some(if populated {
                        "builder test"
                    } else {
                        "Panel dispatch change"
                    }),
                ),
                (
                    "panel_alias",
                    if populated {
                        Some("builder-alias")
                    } else if panel {
                        None
                    } else {
                        Some("claude-delegate")
                    },
                ),
                ("panel_room_id", Some("room-1")),
                ("panel_room_participant", populated.then_some("call-p1")),
                ("panel_lease_id", populated.then_some("lease-1")),
                ("panel_continues", populated.then_some("att_cafe")),
            ] {
                assert_eq!(refs.get(key).text(), expected, "{mode}: {key}");
            }

            let index_raw = fs::read_to_string(repo.0.join(".oms/artifacts/index.jsonl")).unwrap();
            let row = V::parse(index_raw.lines().last().unwrap().as_bytes()).unwrap();
            let is_hex64 = |s: &str| {
                s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            };
            assert!(
                row.get("event_id")
                    .text()
                    .is_some_and(|v| v.starts_with("evt_"))
            );
            assert_eq!(row.get("task_id").text(), Some("t1"));
            assert!(row.get("artifact_sha256").text().is_some_and(is_hex64));
            assert!(row.get("patch_sha256").text().is_some_and(is_hex64));
            assert_eq!(row.get("verify_exit").py(), "0");
            assert!(
                !fs::read(repo.0.join(row.get("patch").text().unwrap()))
                    .unwrap()
                    .is_empty()
            );

            // Advisory enrollment uses the same metadata boundary while retaining
            // its own defaults and the ambient task fallback when CLI is absent.
            let values = args(&[
                "--repo",
                repo.0.to_str().unwrap(),
                "--providers",
                "claude",
                "--model",
                "fixture",
                "--strategy",
                "advisor",
                "--no-failures",
                "--no-task",
                "--no-memory",
                "Read the change",
            ]);
            let out = run_with_source(
                "peer-ask",
                &values,
                &repo.0,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                &stop,
            );
            assert_eq!(out.0, 0, "{mode}: {}", message(&out));
            let events = fs::read_to_string(repo.0.join(".oms/lifecycle/events.jsonl")).unwrap();
            let created = events
                .lines()
                .map(|line| V::parse(line.as_bytes()).unwrap())
                .find(|event| {
                    event.get("event_type").text() == Some("attempt.created")
                        && event.get("tool").text() == Some("peer-ask")
                })
                .expect("advisory attempt.created event");
            assert_eq!(created.get("task_id").text(), Some("ambient-task"));
            assert_eq!(
                created.get("parent_attempt_id").text(),
                Some("att_main1234")
            );
            let refs = created.get("refs");
            for (key, expected) in [
                ("panel_model", Some("fixture")),
                (
                    "panel_role",
                    Some(if populated { "worker" } else { "advisor" }),
                ),
                (
                    "panel_label",
                    Some(if populated {
                        "builder test"
                    } else {
                        "peer-ask (claude)"
                    }),
                ),
                (
                    "panel_alias",
                    Some(if populated {
                        "builder-alias"
                    } else {
                        "claude-peer-ask"
                    }),
                ),
                (
                    "panel_access",
                    if populated {
                        Some("write")
                    } else if panel {
                        Some("read")
                    } else {
                        None
                    },
                ),
                ("panel_owner", populated.then_some("antigravity")),
                ("panel_purpose", populated.then_some("implement")),
                ("panel_workload", populated.then_some("routine")),
                ("panel_room_id", Some("room-1")),
                ("panel_room_participant", populated.then_some("call-p1")),
                ("panel_effort", None),
                ("panel_strategy", None),
                ("panel_lease_id", None),
                ("panel_continues", None),
            ] {
                assert_eq!(refs.get(key).text(), expected, "{mode}: advisory {key}");
            }
        },
    );
}

fn account_isolated(name: &str, extra: &[(String, String)], body: impl FnOnce(&Path)) {
    if let (Some(repo_dir), Some(tool)) = (
        std::env::var_os("OMS_ACCOUNT_DISPATCH_REPO"),
        std::env::var("OMS_ACCOUNT_DISPATCH_TOOL").ok(),
    ) {
        let mut values = vec!["--repo", repo_dir.to_str().unwrap(), "--to", "codex"];
        if tool == "peer-delegate" {
            values.extend([
                "--read-only",
                "--model",
                "fixture",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
            ]);
        } else {
            values.extend([
                "--model",
                "fixture",
                "--strategy",
                "advisor",
                "--no-failures",
                "--no-task",
                "--no-memory",
            ]);
        }
        values.push("Implicit cap probe");
        let out = run_with_source(
            &tool,
            &args(&values),
            Path::new(&repo_dir),
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &STOP,
        );
        assert_eq!(out.0, 0, "{}", message(&out));
        return;
    }
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        body(Path::new(&std::env::var_os("HOME").unwrap()));
        return;
    }
    let home = Repo::new();
    let bin = home.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let claude = "printf 'claude|%s\\n' \"$CLAUDE_CONFIG_DIR\" >> \"$HOME/calls.log\"\ncase \"$CLAUDE_CONFIG_DIR\" in *overflow-a1) echo 'rate limit reached' >&2; exit 1;; esac\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}\\n'\n";
    let pi = "printf 'pi|%s|%s\\n' \"$PI_CODING_AGENT_DIR\" \"${PI_CODING_AGENT_SESSION_DIR-unset}\" >> \"$HOME/calls.log\"\necho ok\n";
    for (file, script) in [
        ("claude", claude),
        ("pi", pi),
        ("agy", "exit 0\n"),
        ("codex", "exit 0\n"),
    ] {
        fs::write(bin.join(file), format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(bin.join(file), fs::Permissions::from_mode(0o755)).unwrap();
    }
    for dir in [
        "acc/overflow-a1",
        "acc/work",
        "acc/pi1",
        "acc/codex1",
        "acc/codex-work",
        ".claude",
        "custom",
    ] {
        fs::create_dir_all(home.0.join(dir)).unwrap();
        fs::set_permissions(home.0.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let h = home.0.display();
    home.write(
        ".config/oh-my-setting/provider-accounts.json",
        format!(
            r#"{{"schema":1,"providers":{{
"claude":{{"policy":"ordered","accounts":[{{"id":"a1","home":"{h}/acc/overflow-a1"}},{{"id":"work","home":"{h}/acc/work"}}]}},
"pi":{{"accounts":[{{"id":"p1","home":"{h}/acc/pi1"}}]}},
"codex":{{"accounts":[{{"id":"work","home":"{h}/acc/codex-work"}}]}}}}}}"#
        ),
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    clean_env_child(&mut command)
        .args(["--exact", name, "--nocapture"])
        .env("HOME", &home.0)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env(CHILD_ENV, name);
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{out}");
    assert!(out.contains("1 passed;"), "child ran no test: {out}");
}
fn account_repo() -> Repo {
    let repo = Repo::new();
    repo.write("README.md", "base\n");
    repo.git(&["add", "README.md"]);
    repo.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "init",
    ]);
    repo
}
fn account_delegate(repo: &Repo, extra: &[&str]) -> Output {
    let mut values = vec!["--repo", repo.0.to_str().unwrap()];
    values.extend_from_slice(extra);
    values.extend([
        "--read-only",
        "--model",
        "fixture",
        "--no-task",
        "--no-memory",
        "--no-ml-context",
        "Account probe",
    ]);
    run_with_source(
        "peer-delegate",
        &args(&values),
        &repo.0,
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
        &STOP,
    )
}
fn account_log(home: &Path) -> Vec<String> {
    fs::read_to_string(home.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.replace(&home.display().to_string(), "$HOME"))
        .collect()
}
fn lifecycle_refs(repo: &Repo, kind: &str) -> Vec<Option<String>> {
    fs::read_to_string(repo.0.join(".oms/lifecycle/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| V::parse(l.as_bytes()).unwrap())
        .filter(|v| v.get("event_type").text() == Some(kind))
        .map(|v| v.get("refs").get("panel_account").text().map(str::to_owned))
        .collect()
}
fn message(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.1),
        String::from_utf8_lossy(&out.2)
    )
}

#[test]
fn provider_accounts_select_pin_overflow_and_validate() {
    account_isolated(
        "provider_accounts_select_pin_overflow_and_validate",
        &[("PI_CODING_AGENT_SESSION_DIR".into(), "elsewhere".into())],
        |home| {
            let repo = account_repo();
            // Auto: first ordered account hits capacity, one retry moves to the next.
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(
                account_log(home),
                ["claude|$HOME/acc/overflow-a1", "claude|$HOME/acc/work"]
            );
            assert_eq!(
                lifecycle_refs(&repo, "attempt.created"),
                [Some("claude:a1".into())]
            );
            let moved = lifecycle_refs(&repo, "attempt.state_changed");
            assert_eq!(moved.last().unwrap().as_deref(), Some("claude:work"));
            let health = fs::read_to_string(repo.0.join(".oms/seat-health.json")).unwrap();
            let health = V::parse(health.as_bytes()).unwrap();
            let seat = health.get("accounts").get("claude:a1");
            assert!(seat.get("cooldown_until").py() != "0", "{}", seat.py());
            assert!(matches!(health.get("accounts").get("a1"), V::Null));
            fs::remove_file(home.join("calls.log")).unwrap();

            // Pinned by flag or by target suffix: no overflow, no silent re-selection.
            let out = account_delegate(&repo, &["--to", "claude", "--account", "a1"]);
            assert_ne!(out.0, 0);
            assert_eq!(account_log(home), ["claude|$HOME/acc/overflow-a1"]);
            let out = account_delegate(&repo, &["--to", "claude:account=work"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "claude|$HOME/acc/work");
            let created = lifecycle_refs(&repo, "attempt.created");
            assert_eq!(created.last().unwrap().as_deref(), Some("claude:work"));

            // pi: account home bound and the session dir override removed.
            let out = account_delegate(&repo, &["--to", "pi", "--account", "p1"]);
            assert!(!message(&out).contains("unavailable"), "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "pi|$HOME/acc/pi1|unset");

            // Validation fails closed before any worker runs.
            let calls = account_log(home).len();
            for (extra, needle) in [
                (
                    &["--to", "claude:account=work", "--account", "a1"][..],
                    "conflicts",
                ),
                (
                    &["--to", "claude", "--account", "Bad_ID"],
                    "invalid provider account id",
                ),
                (
                    &["--to", "claude", "--account", ""],
                    "invalid provider account id",
                ),
                (&["--to", "claude", "--account", "zz"], "unavailable"),
                (&["--to", "pi", "--account", "work"], "unavailable"),
                (
                    &["--to", "antigravity", "--account", "x"],
                    "isolation is unproved",
                ),
                (
                    &["--to", "claude:account=a1:account=a1"],
                    "invalid provider account id",
                ),
            ] {
                let out = account_delegate(&repo, extra);
                assert_ne!(out.0, 0, "{extra:?}");
                assert!(
                    message(&out).contains(needle),
                    "{extra:?}: {}",
                    message(&out)
                );
            }
            assert_eq!(account_log(home).len(), calls);
            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            let out = run(
                &repo,
                source,
                "consult",
                &[
                    "--to",
                    "claude:account=work",
                    "--to",
                    "claude:account=missing",
                    "--no-task",
                    "--no-memory",
                    "--no-ml-context",
                    "q",
                ],
            );
            assert_ne!(out.0, 0);
            assert!(message(&out).contains("unavailable"), "{}", message(&out));
            assert_eq!(account_log(home).len(), calls);
            let out = run(
                &repo,
                source,
                "consult",
                &[
                    "--to",
                    "claude:account=work",
                    "--to",
                    "claude:account=missing:optional",
                    "--no-task",
                    "--no-memory",
                    "--no-ml-context",
                    "q",
                ],
            );
            assert!(
                !message(&out).contains("destination already exists"),
                "{}",
                message(&out)
            );
            assert!(message(&out).contains("optional"), "{}", message(&out));

            let council = run(
                &repo,
                Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
                "peer-ask",
                &[
                    "--providers",
                    "claude,pi",
                    "--account",
                    "work",
                    "--dry-run",
                    "q",
                ],
            );
            assert!(message(&council).contains("exactly one provider"));
            let out = run(
                &repo,
                source,
                "consult",
                &[
                    "--to",
                    "claude:account=work",
                    "--to",
                    "claude:account=a1",
                    "--no-task",
                    "--no-memory",
                    "--no-ml-context",
                    "q",
                ],
            );
            assert!(
                !message(&out).contains("destination already exists"),
                "{}",
                message(&out)
            );
            // Same id on two providers is legal; one canonical home on two is not.
            let config = home.join(".config/oh-my-setting/provider-accounts.json");
            let good = fs::read_to_string(&config).unwrap();
            let out = account_delegate(&repo, &["--to", "codex", "--account", "work"]);
            assert!(!message(&out).contains("duplicate"), "{}", message(&out));
            for (from, to, needle) in [
                (
                    "acc/codex-work",
                    "acc/work",
                    "duplicate provider account id or canonical home",
                ),
                ("\"pi\":{", "\"antigravity\":{", "isolation is unproved"),
                (
                    "\"schema\":1",
                    "\"schema\":2",
                    "unsupported provider accounts schema",
                ),
            ] {
                fs::write(&config, good.replace(from, to)).unwrap();
                let out = account_delegate(&repo, &["--to", "claude", "--account", "work"]);
                assert!(
                    message(&out).contains(needle),
                    "{needle}: {}",
                    message(&out)
                );
            }
            fs::write(&config, "{bad").unwrap();
            let out = account_delegate(&repo, &["--to", "antigravity", "--dry-run"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            let out = account_delegate(
                &repo,
                &["--to", "antigravity", "--account", "work", "--dry-run"],
            );
            assert_ne!(out.0, 0);
            assert!(message(&out).contains("unproved"), "{}", message(&out));
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_ne!(out.0, 0);
            assert!(
                message(&out).contains("invalid provider accounts"),
                "{}",
                message(&out)
            );
            // Claude's own registry supplies accounts after an implicit default.
            fs::write(&config, "{\"schema\":1,\"providers\":{}}").unwrap();
            fs::write(
                home.join(".config/oh-my-setting/provider-accounts.claude.json"),
                format!(
                    "{{\"schema\":1,\"provider\":\"claude\",\"accounts\":[{{\"id\":\"w2\",\"home\":\"{}/acc/work\"}}]}}",
                    home.display()
                ),
            )
            .unwrap();
            fs::remove_file(home.join("calls.log")).unwrap();
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home), ["claude|$HOME/.claude"]);
            let out = account_delegate(&repo, &["--to", "claude", "--account", "w2"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "claude|$HOME/acc/work");
            let out = account_delegate(&repo, &["--to", "claude", "--account", "default"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "claude|$HOME/.claude");
            // Missing or non-array Claude-private accounts fail closed instead of looking empty.
            let claude_private = home.join(".config/oh-my-setting/provider-accounts.claude.json");
            let saved_claude_private = fs::read(&claude_private).unwrap();
            for bad in [
                r#"{"schema":1,"provider":"claude"}"#,
                r#"{"schema":1,"provider":"claude","accounts":"invalid"}"#,
            ] {
                fs::write(&claude_private, bad).unwrap();
                let _ = fs::remove_file(home.join("calls.log"));
                let err_out = account_delegate(&repo, &["--to", "claude"]);
                assert_ne!(err_out.0, 0, "{bad}");
                assert!(
                    message(&err_out).contains("provider accounts"),
                    "{bad}: {}",
                    message(&err_out)
                );
                assert!(account_log(home).is_empty(), "{bad}");
            }
            // Another provider owning the standard home must block both pinned default and implicit fallback.
            fs::remove_file(&claude_private).unwrap();
            fs::write(
                &config,
                format!(
                    "{{\"schema\":1,\"providers\":{{\"codex\":{{\"accounts\":[{{\"id\":\"steal\",\"home\":\"{}/.claude\"}}]}},\"claude\":{{\"accounts\":[{{\"id\":\"missing_w\",\"home\":\"{}/acc/missing-claude\"}}]}}}}}}",
                    home.display(),
                    home.display(),
                ),
            )
            .unwrap();
            let _ = fs::remove_file(home.join("calls.log"));
            let out = account_delegate(&repo, &["--to", "claude", "--account", "default"]);
            assert_ne!(out.0, 0, "{}", message(&out));
            assert!(account_log(home).is_empty(), "{}", message(&out));
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_ne!(out.0, 0, "{}", message(&out));
            assert!(account_log(home).is_empty(), "{}", message(&out));
            fs::write(
                &config,
                format!(
                    "{{\"schema\":1,\"providers\":{{\"codex\":{{\"accounts\":[{{\"id\":\"steal\",\"home\":\"{}/.claude\"}}]}}}}}}",
                    home.display(),
                ),
            )
            .unwrap();
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_ne!(out.0, 0, "{}", message(&out));
            assert!(account_log(home).is_empty(), "{}", message(&out));
            fs::write(&claude_private, saved_claude_private).unwrap();
            // Explicit unpinned routes fall back to the caller home when no registered account qualifies,
            // provided the fallback home itself passes safety and model checks.
            let default_codex = home.join(".codex");
            fs::create_dir_all(&default_codex).unwrap();
            fs::set_permissions(&default_codex, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                &config,
                format!(
                    "{{\"schema\":1,\"providers\":{{\"codex\":{{\"accounts\":[{{\"id\":\"codex1\",\"home\":\"{}/acc/codex-missing\"}}]}}}}}}",
                    home.display()
                ),
            )
            .unwrap();
            let out = account_delegate(&repo, &["--to", "codex"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            let out = account_delegate(&repo, &["--to", "codex", "--account", "codex1"]);
            assert_ne!(out.0, 0);
            assert!(message(&out).contains("unavailable"), "{}", message(&out));
            fs::set_permissions(&default_codex, fs::Permissions::from_mode(0o777)).unwrap();
            let unsafe_fallback = account_delegate(&repo, &["--to", "codex"]);
            assert_ne!(unsafe_fallback.0, 0);
            assert!(
                message(&unsafe_fallback).contains("no eligible codex account"),
                "{}",
                message(&unsafe_fallback)
            );
            fs::set_permissions(&default_codex, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                default_codex.join("models_cache.json"),
                r#"{"models":[{"slug":"other-model"}]}"#,
            )
            .unwrap();
            let model_excluded = account_delegate(&repo, &["--to", "codex"]);
            assert_ne!(model_excluded.0, 0);
            assert!(
                message(&model_excluded).contains("no eligible codex account"),
                "{}",
                message(&model_excluded)
            );
            fs::remove_file(default_codex.join("models_cache.json")).unwrap();

            // Implicit panel dispatch preserves the 90% account quota cap so a cooling 10% seat
            // beats a healthy 95% seat, while explicit --to (or direct CLI) waives the cap.
            for dir in ["acc/codex-over/sessions", "acc/codex-cool/sessions"] {
                fs::create_dir_all(home.join(dir)).unwrap();
                fs::set_permissions(
                    home.join(dir.trim_end_matches("/sessions")),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            fs::write(
                home.join("bin/codex"),
                "#!/bin/sh\nprintf 'codex|%s\\n' \"$CODEX_HOME\" >> \"$HOME/calls.log\"\necho 'Recommendation: proceed'\n",
            )
            .unwrap();
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let date_out = Command::new("date")
                .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
                .output()
                .unwrap();
            let ts = String::from_utf8(date_out.stdout).unwrap();
            let ts = ts.trim();
            for (acc, used) in [("acc/codex-over", 95), ("acc/codex-cool", 10)] {
                fs::write(
                    home.join(acc).join("sessions/usage.jsonl"),
                    format!(
                        "{{\"timestamp\":\"{ts}\",\"rate_limits\":{{\"primary\":{{\"window_minutes\":10080,\"used_percent\":{used},\"resets_at\":{}}}}}}}\n",
                        now_secs + 3600
                    ),
                )
                .unwrap();
            }
            fs::write(
                &config,
                format!(
                    "{{\"schema\":1,\"providers\":{{\"codex\":{{\"accounts\":[{{\"id\":\"over\",\"home\":\"{h}/acc/codex-over\"}},{{\"id\":\"cool\",\"home\":\"{h}/acc/codex-cool\"}}]}}}}}}",
                    h = home.display()
                ),
            )
            .unwrap();
            let cap_repo = account_repo();
            for (explicit_flag, expected) in [
                ("0", "codex|$HOME/acc/codex-cool"),
                ("1", "codex|$HOME/acc/codex-over"),
            ] {
                for tool in ["peer-delegate", "advise"] {
                    cap_repo.write(
                        ".oms/seat-health.json",
                        format!(
                            "{{\"schema\":1,\"accounts\":{{\"codex:cool\":{{\"cooldown_until\":{},\"last_error_ts\":{}}}}}}}",
                            now_secs + 3600,
                            now_secs
                        ),
                    );
                    let _ = fs::remove_file(home.join("calls.log"));
                    let child = Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "provider_accounts_select_pin_overflow_and_validate",
                            "--nocapture",
                        ])
                        .env("HOME", home)
                        .env("PATH", std::env::var_os("PATH").unwrap())
                        .env("OMS_PANEL_DISPATCH", "1")
                        .env("OMS_PANEL_EXPLICIT_TARGET", explicit_flag)
                        .env("OMS_ACCOUNT_DISPATCH_REPO", &cap_repo.0)
                        .env("OMS_ACCOUNT_DISPATCH_TOOL", tool)
                        .output()
                        .unwrap();
                    assert!(
                        child.status.success(),
                        "explicit={explicit_flag} tool={tool}: {}{}",
                        String::from_utf8_lossy(&child.stdout),
                        String::from_utf8_lossy(&child.stderr)
                    );
                    assert_eq!(
                        account_log(home),
                        [expected],
                        "explicit={explicit_flag} tool={tool}"
                    );
                }
            }
            // Pinned account with a model-excluding cache must refuse launch.
            fs::write(
                home.join("acc/codex-over/models_cache.json"),
                r#"{"models":[{"slug":"other-model"}]}"#,
            )
            .unwrap();
            let _ = fs::remove_file(home.join("calls.log"));
            let excluded_pin = account_delegate(&cap_repo, &["--to", "codex", "--account", "over"]);
            assert_ne!(excluded_pin.0, 0);
            assert!(
                message(&excluded_pin).contains("unavailable"),
                "{}",
                message(&excluded_pin)
            );
            assert!(account_log(home).is_empty());
        },
    );
}

#[test]
fn provider_reader_rollout_keeps_legacy_intents_and_usage() {
    account_isolated(
        "provider_reader_rollout_keeps_legacy_intents_and_usage",
        &[("OMS_RESUME_SESSION".into(), "continuity-thread".into())],
        |home| {
            use oms_command_telemetry::store::{dispatch, physical, usage};
            let repo = account_repo();
            let config = home.join(".config/oh-my-setting/provider-accounts.json");
            let original = home.join("acc/codex-work");
            let replacement = home.join("acc/codex-replacement");
            fs::create_dir(&replacement).unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(home.join("bin/codex"), concat!(
                "#!/bin/sh\n",
                "printf 'codex|%s\\n' \"$CODEX_HOME\" >> \"$HOME/calls.log\"\n",
                "if [ -f \"$HOME/replace-account-home\" ]; then\n",
                "  mv \"$CODEX_HOME\" \"$CODEX_HOME.during-call\" || exit 81\n",
                "  mkdir -m 700 \"$CODEX_HOME\" || exit 82\n",
                "fi\n",
                "n=$(cat \"$HOME/usage-total\")\n",
                "thread=continuity-thread\n",
                "if [ -f \"$HOME/replace-account-home\" ] && [ \"$(cat \"$HOME/replace-account-home\")\" = foreign ]; then thread=foreign-thread; fi\n",
                "printf '{\"type\":\"thread.started\",\"thread_id\":\"%s\"}\\n' \"$thread\"\n",
                "printf '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}\\n'\n",
                "printf '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":%s,\"output_tokens\":0,\"cached_input_tokens\":0,\"total_tokens\":%s}}\\n' \"$n\" \"$n\"\n",
                "if [ -f \"$HOME/replace-account-home\" ]; then echo 'rate limit reached' >&2; exit 1; fi\n",
            )).unwrap();
            let invoke = |total: u64| {
                fs::write(home.join("usage-total"), total.to_string()).unwrap();
                account_delegate(
                    &repo,
                    &[
                        "--to",
                        "codex",
                        "--account",
                        "work",
                        "--fallback-model",
                        "fallback-fixture",
                    ],
                )
            };
            let assert_latest = || {
                let projected = usage::load(&repo.0).unwrap();
                let call = projected.calls.last().unwrap();
                assert_eq!(call.receipt.identity.account.as_deref(), Some("work"));
                assert_eq!(call.receipt.identity.account_scope_sha256, None);
                assert_eq!(call.receipt.phase, usage::Phase::Unknown);
                assert_eq!(call.receipt.usage, usage::Usage::default());
                assert_eq!(call.tokens, None);
                let attempt = call.receipt.identity.attempt_id.as_deref().unwrap();
                assert_eq!(
                    projected
                        .attributed_calls()
                        .iter()
                        .find(|row| row.attempt_id == attempt)
                        .unwrap()
                        .usage
                        .total_tokens,
                    None
                );
                let intents = physical::load(&repo.0, &STOP).unwrap();
                assert_eq!(
                    intents
                        .iter()
                        .find(|intent| intent.identity.call_id == call.receipt.identity.call_id)
                        .unwrap()
                        .identity,
                    call.receipt.identity
                );
                let registry =
                    V::parse(&fs::read(repo.0.join(dispatch::REGISTRY)).unwrap()).unwrap();
                let raw_call = registry
                    .get("physical")
                    .get("calls")
                    .get(&call.receipt.identity.call_id);
                assert_eq!(raw_call.get("schema"), &V::Num("1".into()));
                assert!(
                    raw_call
                        .get("request")
                        .get("identity")
                        .get("account_scope_sha256")
                        == &V::Null
                );
            };
            fs::set_permissions(&original, fs::Permissions::from_mode(0o770)).unwrap();
            let invalid = invoke(90);
            assert_ne!(invalid.0, 0, "{}", message(&invalid));
            assert!(account_log(home).is_empty());
            assert!(physical::load(&repo.0, &STOP).unwrap().is_empty());
            assert!(usage::load(&repo.0).unwrap().calls.is_empty());
            fs::set_permissions(&original, fs::Permissions::from_mode(0o700)).unwrap();

            let replace_during_call = |total, reported_session| {
                let before = account_log(home).len();
                fs::write(home.join("replace-account-home"), reported_session).unwrap();
                let interrupted = invoke(total);
                fs::remove_file(home.join("replace-account-home")).unwrap();
                assert_ne!(interrupted.0, 0, "{}", message(&interrupted));
                assert_eq!(account_log(home).len(), before + 1);
                let output = String::from_utf8_lossy(&interrupted.1);
                let report = fs::read_to_string(output.lines().next().unwrap()).unwrap();
                assert!(
                    report.contains("home changed during invocation"),
                    "{report}"
                );
            };
            let first = invoke(100);
            assert_eq!(first.0, 0, "{}", message(&first));
            assert_latest();
            // Ordinary contents change; the directory generation remains stable.
            fs::write(original.join("fixture-config"), "updated\n").unwrap();
            let second = invoke(130);
            assert_eq!(second.0, 0, "{}", message(&second));
            assert_latest();
            let registered = fs::read_to_string(&config).unwrap();
            fs::write(
                &config,
                registered.replace("acc/codex-work", "acc/codex-replacement"),
            )
            .unwrap();
            let changed = invoke(500);
            assert_eq!(changed.0, 0, "{}", message(&changed));
            assert_latest();
            let continued = invoke(540);
            assert_eq!(continued.0, 0, "{}", message(&continued));
            assert_latest();
            // Identical canonical spelling is insufficient after replacement.
            fs::rename(&replacement, home.join("acc/codex-replacement-old")).unwrap();
            fs::create_dir(&replacement).unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
            let replaced = invoke(600);
            assert_eq!(replaced.0, 0, "{}", message(&replaced));
            assert_latest();
            let continued = invoke(650);
            assert_eq!(continued.0, 0, "{}", message(&continued));
            assert_latest();
            // A replacement can report another thread. Keep the exact intent's
            // unknown liability and cleanup even with a capacity-looking error.
            replace_during_call(700, "foreign");
            assert_latest();
            let projection = usage::load(&repo.0).unwrap();
            let receipt = &projection.calls.last().unwrap().receipt;
            assert_eq!(
                receipt.cumulative.as_ref().unwrap().session_id,
                "continuity-thread"
            );
            assert!(receipt.source_sha256.is_some());
            assert_eq!(receipt.usage, usage::Usage::default());
            assert_eq!(receipt.cleanup_complete, Some(true));
            let after = invoke(730);
            assert_eq!(after.0, 0, "{}", message(&after));
            assert_latest();
            let continued = invoke(750);
            assert_eq!(continued.0, 0, "{}", message(&continued));
            assert_latest();
            let raw = fs::read_to_string(repo.0.join(usage::RECEIPTS)).unwrap();
            assert!(
                !raw.contains(&home.display().to_string()),
                "home paths must not enter the ledger"
            );
            for line in raw.lines() {
                let row = V::parse(line.as_bytes()).unwrap();
                assert_eq!(row.get("schema"), &V::Num("2".into()));
                assert!(!line.contains("account_scope_sha256"));
            }
            assert_eq!(usage::load(&repo.0).unwrap().calls.len(), 9);
        },
    );
}

#[test]
fn provider_reader_rollout_retains_independent_named_usage() {
    account_isolated(
        "provider_reader_rollout_retains_independent_named_usage",
        &[],
        |home| {
            use oms_command_telemetry::store::usage;
            let repo = account_repo();
            let codex_home = home.join("acc/codex-work");
            if fs::metadata(codex_home).unwrap().created().is_err() {
                // These positive controls require the same private generation proof as production.
                return;
            }
            for (provider, script) in [
                (
                    "codex",
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' ",
                        "'{\"type\":\"thread.started\",\"thread_id\":\"fresh-thread\"}' ",
                        "'{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}' ",
                        "'{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"cached_input_tokens\":0,\"total_tokens\":13}}'\n",
                    ),
                ),
                (
                    "claude",
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' ",
                        "'{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}'\n",
                    ),
                ),
            ] {
                fs::write(home.join("bin").join(provider), script).unwrap();
                let out = account_delegate(&repo, &["--to", provider, "--account", "work"]);
                assert_eq!(out.0, 0, "{}", message(&out));
                let ledger = usage::load(&repo.0).unwrap();
                let call = ledger.calls.last().unwrap();
                assert_eq!(call.receipt.identity.provider, provider);
                assert_eq!(call.receipt.identity.account.as_deref(), Some("work"));
                assert_eq!(call.receipt.identity.account_scope_sha256, None);
                assert_eq!(call.receipt.phase, usage::Phase::Final);
                assert_eq!(call.tokens, Some(13));
                assert_eq!(
                    call.receipt.cumulative.as_ref().map(|s| s.baseline_unknown),
                    (provider == "codex").then_some(false)
                );
            }
        },
    );
}

#[test]
fn claude_binding_refuses_credential_overrides_and_resume_guesses() {
    let key = format!("ANTHROPIC_{}", "API_KEY");
    account_isolated(
        "claude_binding_refuses_credential_overrides_and_resume_guesses",
        &[
            (key, "fixture".into()),
            ("OMS_RESUME_SESSION".into(), "sess-1".into()),
        ],
        |home| {
            let repo = account_repo();
            let out = account_delegate(&repo, &["--to", "claude", "--account", "work"]);
            assert_ne!(out.0, 0);
            assert!(
                message(&out).contains("credential environment"),
                "{}",
                message(&out)
            );
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_ne!(out.0, 0);
            assert!(message(&out).contains("ambiguous"), "{}", message(&out));
            assert!(account_log(home).is_empty());
            // The default home keeps its own credentials, so the guard does not apply.
            let out = account_delegate(&repo, &["--to", "claude", "--account", "default"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home), ["claude|$HOME/.claude"]);
            // A single non-default registered home is still ambiguous.
            let single = home.join(".config/oh-my-setting/provider-accounts.json");
            fs::write(
                single,
                format!(
                    "{{\"schema\":1,\"providers\":{{\"pi\":{{\"accounts\":[{{\"id\":\"p1\",\"home\":\"{}/acc/pi1\"}}]}}}}}}",
                    home.display()
                ),
            )
            .unwrap();
            let out = account_delegate(&repo, &["--to", "pi"]);
            assert!(message(&out).contains("ambiguous"), "{}", message(&out));
            let default_pi = home.join(".pi/agent");
            fs::create_dir_all(&default_pi).unwrap();
            fs::set_permissions(&default_pi, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(home.join(".config/oh-my-setting/provider-accounts.json"), format!("{{\"schema\":1,\"providers\":{{\"pi\":{{\"accounts\":[{{\"id\":\"only\",\"home\":\"{}\"}}]}}}}}}", default_pi.display())).unwrap();
            let out = account_delegate(&repo, &["--to", "pi"]);
            assert!(!message(&out).contains("ambiguous"), "{}", message(&out));
            assert_eq!(
                account_log(home).last().unwrap(),
                "pi|$HOME/.pi/agent|unset"
            );
        },
    );
}

#[test]
fn caller_home_variable_wins_unless_an_account_is_requested() {
    if let Some(repo_dir) = std::env::var_os("OMS_RESUME_HOME_REPO") {
        let values = [
            "--repo",
            repo_dir.to_str().unwrap(),
            "--to",
            "claude",
            "--read-only",
            "--model",
            "fixture",
            "--no-task",
            "--no-memory",
            "--no-ml-context",
            "Account probe",
        ];
        let out = run_with_source(
            "peer-delegate",
            &args(&values),
            Path::new(&repo_dir),
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &STOP,
        );
        if std::env::var_os("OMS_RESUME_EXPECT_FAIL").is_some() {
            assert_ne!(out.0, 0, "{}", message(&out));
            if let Ok(expect) = std::env::var("OMS_RESUME_EXPECT_CONTAINS") {
                assert!(message(&out).contains(&expect), "{}", message(&out));
            }
            return;
        }
        assert_eq!(out.0, 0, "{}", message(&out));
        return;
    }
    let custom = "CLAUDE_CONFIG_DIR".to_owned();
    let env_name = format!("ANTHROPIC_{}", "API_KEY");
    account_isolated(
        "caller_home_variable_wins_unless_an_account_is_requested",
        &[(custom, "custom".into())],
        |home| {
            let repo = account_repo();
            let out = account_delegate(&repo, &["--to", "claude"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home), ["claude|custom"]);
            assert_eq!(lifecycle_refs(&repo, "attempt.created"), [None]);
            let out = account_delegate(&repo, &["--to", "claude", "--account", "work"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "claude|$HOME/acc/work");
            let out = account_delegate(&repo, &["--to", "claude", "--account", "default"]);
            assert_eq!(out.0, 0, "{}", message(&out));
            assert_eq!(account_log(home).last().unwrap(), "claude|$HOME/.claude");

            // Registered non-default caller override must honor Claude credential isolation,
            // and a home claimed by another provider must fail closed instead of bypassing bind.
            let _ = fs::remove_file(home.join("calls.log"));
            let cred_collision = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "caller_home_variable_wins_unless_an_account_is_requested",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("CLAUDE_CONFIG_DIR", home.join("acc/work"))
                .env(&env_name, "fixture")
                .env("OMS_RESUME_HOME_REPO", &repo.0)
                .env("OMS_RESUME_EXPECT_FAIL", "1")
                .env("OMS_RESUME_EXPECT_CONTAINS", "credential environment")
                .env_remove("OMS_RESUME_SESSION")
                .output()
                .unwrap();
            assert!(
                cred_collision.status.success(),
                "{}{}",
                String::from_utf8_lossy(&cred_collision.stdout),
                String::from_utf8_lossy(&cred_collision.stderr)
            );
            assert!(account_log(home).is_empty());

            let cross_provider = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "caller_home_variable_wins_unless_an_account_is_requested",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("CLAUDE_CONFIG_DIR", home.join("acc/codex-work"))
                .env("OMS_RESUME_HOME_REPO", &repo.0)
                .env("OMS_RESUME_EXPECT_FAIL", "1")
                .env_remove(&env_name)
                .env_remove("OMS_RESUME_EXPECT_CONTAINS")
                .env_remove("OMS_RESUME_SESSION")
                .output()
                .unwrap();
            assert!(
                cross_provider.status.success(),
                "{}{}",
                String::from_utf8_lossy(&cross_provider.stdout),
                String::from_utf8_lossy(&cross_provider.stderr)
            );
            assert!(account_log(home).is_empty());

            let reg_repo = account_repo();
            let _ = fs::remove_file(home.join("calls.log"));
            let reg_override = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "caller_home_variable_wins_unless_an_account_is_requested",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("CLAUDE_CONFIG_DIR", home.join("acc/work"))
                .env("OMS_RESUME_HOME_REPO", &reg_repo.0)
                .env_remove(&env_name)
                .env_remove("OMS_RESUME_EXPECT_FAIL")
                .env_remove("OMS_RESUME_EXPECT_CONTAINS")
                .env_remove("OMS_RESUME_SESSION")
                .output()
                .unwrap();
            assert!(
                reg_override.status.success(),
                "{}{}",
                String::from_utf8_lossy(&reg_override.stdout),
                String::from_utf8_lossy(&reg_override.stderr)
            );
            assert_eq!(account_log(home), ["claude|$HOME/acc/work"]);
            assert_eq!(
                lifecycle_refs(&reg_repo, "attempt.created"),
                [Some("claude:work".into())]
            );
            let health = fs::read_to_string(reg_repo.0.join(".oms/seat-health.json")).unwrap();
            assert!(health.contains("\"claude:work\""), "{health}");
            assert!(!health.contains("\"claude\":"), "{health}");

            // Unpinned resume must also honor a caller home override instead of binding ~/.claude.
            let cfg = home.join(".config/oh-my-setting");
            fs::write(
                cfg.join("provider-accounts.json"),
                format!(
                    "{{\"schema\":1,\"providers\":{{\"claude\":{{\"accounts\":[{{\"id\":\"only\",\"home\":\"{}/.claude\"}}]}}}}}}",
                    home.display()
                ),
            )
            .unwrap();
            let _ = fs::remove_file(home.join("calls.log"));
            let resumed = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "caller_home_variable_wins_unless_an_account_is_requested",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("CLAUDE_CONFIG_DIR", "custom")
                .env("OMS_RESUME_SESSION", "sess-1")
                .env("OMS_RESUME_HOME_REPO", &repo.0)
                .output()
                .unwrap();
            assert!(
                resumed.status.success(),
                "{}{}",
                String::from_utf8_lossy(&resumed.stdout),
                String::from_utf8_lossy(&resumed.stderr)
            );
            assert_eq!(account_log(home), ["claude|custom"]);
        },
    );
}

#[test]
fn session_handoff_prompt_projects_bounded_native_reads() {
    isolated(
        "session_handoff_prompt_projects_bounded_native_reads",
        || {
            use oms_command_peer::{obj, process::HandoffProjection};

            struct Reads {
                mode: &'static str,
                calls: Mutex<Vec<(HandoffProjection, Duration)>>,
            }
            impl Reads {
                fn new(mode: &'static str) -> Self {
                    Self {
                        mode,
                        calls: Mutex::new(Vec::new()),
                    }
                }
            }
            impl Executor for Reads {
                fn experience_context(
                    &self,
                    _repo: &Path,
                    query: &str,
                    _interrupted: &AtomicBool,
                ) -> oms_command_peer::Result<serde_json::Value> {
                    if self.mode != "autopilot" {
                        return Err(oms_command_peer::Error::new(
                            2,
                            "fixture experience unavailable",
                        ));
                    }
                    assert!(query.contains("Plan continuity"));
                    Ok(serde_json::json!({"schema":1,"authority":"advisory",
                        "entries":[{"outcome":"valid negative measurement","source_sha256":"d".repeat(64)}]}))
                }
                fn execute(
                    &self,
                    request: Invocation,
                    interrupted: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    assert_eq!(request.argv[0], "git", "unexpected provider execution");
                    Native.execute(request, interrupted)
                }
                fn handoff_projection(
                    &self,
                    projection: HandoffProjection,
                    repo: &Path,
                    state: &Path,
                    timeout: Duration,
                    interrupted: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    assert_eq!(repo, state);
                    assert!(!timeout.is_zero() && timeout <= Duration::from_secs(40));
                    self.calls.lock().unwrap().push((projection, timeout));
                    if self.mode == "unavailable" {
                        return Err(oms_command_peer::Error::new(2, "fixture unavailable"));
                    }
                    if self.mode == "interrupted" {
                        interrupted.store(true, Ordering::Release);
                        return Err(oms_command_peer::Error::new(130, "fixture interrupted"));
                    }
                    if self.mode == "autopilot_interrupted"
                        && projection == HandoffProjection::AutopilotStatus
                    {
                        return Err(oms_command_peer::Error::new(
                            130,
                            "fixture late cancellation",
                        ));
                    }
                    let mut stdout = match projection {
                    HandoffProjection::TaskStatus => {
                        br#"{"schema":1,"status":"active","verification":"not_verified"}"#.to_vec()
                    }
                    HandoffProjection::EvidenceMatrix => {
                        br#"{"criteria":{"items":[{"criterion":"tests","status":"unknown","source":"receipt_missing"}],"omitted":0}}"#.to_vec()
                    }
                    HandoffProjection::NextStep => {
                        br#"{"actions":{"items":[{"next_action":"run verification","reason":"receipt missing"}],"omitted":0}}"#.to_vec()
                    }
                    HandoffProjection::JournalDecisions => {
                        br#"{"decisions":[{"event_id":"decision_1","text":"keep native context"}],"omitted":0}"#.to_vec()
                    }
                    HandoffProjection::ReviewFindings => {
                        br#"[{"id":"finding_1","status":"open","summary":"review freshness"}]"#.to_vec()
                    }
                    HandoffProjection::AutopilotStatus => {
                        serde_json::to_vec(&serde_json::json!({"next_action":{"schema":1,"kind":"autopilot-next-action",
                            "owner_id":"fixture-owner","stage":"parked","receipt_sha256":"a".repeat(64),
                            "decision_sha256":"b".repeat(64),"action":"resume","reason":"exact terminal result remains unconsumed",
                            "wait":{"status":"terminal","result_sha256":"c".repeat(64)},"budget":{"remaining_cycles":2},
                            "commands":[["oms","autopilot","status"]]}})).unwrap()
                    }
                };
                    if matches!(self.mode, "empty" | "omitted_only" | "omitted_one")
                        && !matches!(
                            projection,
                            HandoffProjection::TaskStatus | HandoffProjection::AutopilotStatus
                        )
                    {
                        let key = match projection {
                            HandoffProjection::EvidenceMatrix => "criteria",
                            HandoffProjection::NextStep => "actions",
                            HandoffProjection::JournalDecisions => "decisions",
                            HandoffProjection::ReviewFindings => "",
                            HandoffProjection::TaskStatus | HandoffProjection::AutopilotStatus => {
                                unreachable!()
                            }
                        };
                        stdout = if key.is_empty() {
                            b"[]".to_vec()
                        } else {
                            format!(
                                r#"{{"{key}":{{"items":[],"omitted":{}}}}}"#,
                                if self.mode == "omitted_only" {
                                    u64::MAX
                                } else if self.mode == "omitted_one" {
                                    1
                                } else {
                                    0
                                }
                            )
                            .into_bytes()
                        };
                    }
                    let mut complete = true;
                    if self.mode == "collections"
                        && !matches!(
                            projection,
                            HandoffProjection::TaskStatus | HandoffProjection::AutopilotStatus
                        )
                    {
                        let (key, name, count, omitted, bounded) = match projection {
                            HandoffProjection::EvidenceMatrix => {
                                ("criteria", "criterion", 24, 2, true)
                            }
                            HandoffProjection::NextStep => ("actions", "action", 20, 3, true),
                            HandoffProjection::JournalDecisions => {
                                ("decisions", "decision", 32, 7, false)
                            }
                            HandoffProjection::ReviewFindings => ("", "finding", 17, 0, false),
                            HandoffProjection::TaskStatus | HandoffProjection::AutopilotStatus => {
                                unreachable!()
                            }
                        };
                        let rows = V::Arr(
                            (0..count)
                                .map(|n| {
                                    obj([
                                        ("id", format!("{name}-{n:02}").into()),
                                        (
                                            "status",
                                            if key.is_empty() { "open" } else { "unknown" }.into(),
                                        ),
                                        ("summary", "complete bounded row ".repeat(10).into()),
                                    ])
                                })
                                .collect(),
                        );
                        let omitted = V::from(omitted as usize);
                        let projection = if key.is_empty() {
                            rows
                        } else if bounded {
                            obj([(key, obj([("items", rows), ("omitted", omitted)]))])
                        } else {
                            obj([(key, rows), ("omitted", omitted)])
                        };
                        stdout = projection.json(false).unwrap();
                        assert!(stdout.len() > 2048 && stdout.len() < 65536);
                    }
                    if self.mode == "blocked_only" && projection == HandoffProjection::NextStep {
                        let tasks = (0..10)
                            .map(|n| {
                                let (id, state, reason) = if n < 2 {
                                    (format!("done-{n}"), "done", "recorded-done")
                                } else {
                                    (
                                        format!("blocked-{:02}", n - 2),
                                        "ready",
                                        "dependencies-unfinished",
                                    )
                                };
                                obj([
                                    ("task_id", id.into()),
                                    ("state", state.into()),
                                    ("canonical_actionable", false.into()),
                                    ("reasons", V::Arr(vec![obj([("code", reason.into())])])),
                                ])
                            })
                            .collect();
                        stdout = obj([
                            (
                                "actions",
                                obj([("items", V::Arr(Vec::new())), ("omitted", 0usize.into())]),
                            ),
                            (
                                "tasks",
                                obj([("items", V::Arr(tasks)), ("omitted", 3usize.into())]),
                            ),
                        ])
                        .json(false)
                        .unwrap();
                    }
                    if projection == HandoffProjection::EvidenceMatrix {
                        match self.mode {
                            "malformed" => {
                                stdout = br#"{"status":"passed","status":"unknown"}"#.to_vec();
                            }
                            "incomplete" => {
                                stdout = br#"{"status":"incomplete_probe"}"#.to_vec();
                                complete = false;
                            }
                            "line_budget" | "read_budget" => {
                                let size = if self.mode == "line_budget" {
                                    3000
                                } else {
                                    65536
                                };
                                stdout = format!(
                                    r#"{{"criteria":{{"items":[{{"oversize_probe":"{}"}}],"omitted":0}}}}"#,
                                    "x".repeat(size)
                                ).into_bytes();
                            }
                            "sensitive" => {
                                let private = ["/ho", "me/private/handoff"].concat();
                                stdout = format!(r#"{{"source":"{private}"}}"#).into_bytes();
                            }
                            _ => {}
                        }
                    }
                    Ok(Completed {
                        code: 0,
                        stdout,
                        stderr: Vec::new(),
                        complete,
                        cleanup_complete: Some(true),
                        owned_cleanup: None,
                        elapsed: Duration::ZERO,
                    })
                }
            }
            let repo = Repo::new();
            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            let stop = AtomicBool::new(false);
            let packet = concat!(
                "# Task\n- task_id: handoff-fixture\n- status: active\n\n",
                "## Goal\nResume the recorded λ goal.\n\n",
                "## Decisions\n- 2026-11-20T00:00:00Z [main] preserve source anchors\n",
            );
            repo.write(".oms/task/current.md", packet);
            repo.write(
                ".oms/plan/tasks.json",
                br#"{"schema":3,"goal":"Plan continuity","accept":"","tasks":{}}"#,
            );
            let call = |reads: &Reads, values: &[&str]| {
                oms_command_peer::run_with_executor(
                    "session-handoff",
                    &args(values),
                    &repo.0,
                    source,
                    &stop,
                    reads,
                )
            };
            let flags = |body: &str, section: &str| {
                body.split(&format!("\n## {section}\n"))
                    .nth(1)
                    .unwrap_or_else(|| panic!("missing {section}: {body}"))
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned()
            };
            let reads = Reads::new("complete");
            let result = call(&reads, &["prompt"]);
            assert_eq!(result.0, 0, "{}", message(&result));
            let body = std::str::from_utf8(&result.1).unwrap();
            for recorded in [
                "plan goal: Plan continuity",
                "task goal: Resume the recorded λ goal.",
                "preserve source anchors",
                "[src: task packet Decisions]",
                "[src: agent-task status --json]",
                "[src: runtime evidence matrix --limit 100]",
                "[src: runtime next --explain --limit 100]",
                "[src: journal show --decisions --json]",
                "[src: peer-review findings list --status open --json]",
                "receipt_missing",
                "not_verified",
                "finding_1",
                "decision_1",
                ".oms/plan/progress.jsonl",
                "Recorded text is a claim, not fresh verification",
            ] {
                assert!(body.contains(recorded), "missing {recorded}: {body}");
            }
            assert!(body.len() <= 4096);
            for section in [
                "Goal",
                "State",
                "Decisions",
                "Open findings",
                "Verified / Not verified",
                "Next step",
                "Pointers",
            ] {
                assert_eq!(flags(body, section), "[-]", "{section}: {body}");
            }
            let calls = reads.calls.lock().unwrap();
            assert_eq!(calls.len(), 5);
            assert!(calls.windows(2).all(|pair| pair[0].1 >= pair[1].1));
            drop(calls);
            assert_eq!(
                fs::read(repo.0.join(".oms/task/current.md")).unwrap(),
                packet.as_bytes()
            );
            assert!(!repo.0.join(".oms/handoffs").exists());

            // A receipt is only a discovery signal here. The checked next
            // action must come from the fixed native status producer, while
            // experience crosses the real peer -> autonomy adapter as advisory.
            repo.write(".oms/plan/autopilot-run.json", b"{}");
            let adaptive = Reads::new("autopilot");
            let result = call(&adaptive, &["prompt", "--max-bytes", "8192"]);
            assert_eq!(result.0, 0, "{}", message(&result));
            let body = std::str::from_utf8(&result.1).unwrap();
            for expected in [
                "fixture-owner",
                "exact terminal result remains unconsumed",
                "remaining_cycles",
                "Advisory prior outcome",
                "valid negative measurement",
                "source-checked journal experience",
            ] {
                assert!(body.contains(expected), "missing {expected}: {body}");
            }
            assert_eq!(
                adaptive
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(projection, _)| *projection == HandoffProjection::AutopilotStatus)
                    .count(),
                1
            );
            let interrupted_read = call(&Reads::new("autopilot_interrupted"), &["prompt"]);
            assert_eq!(interrupted_read.0, 130, "{}", message(&interrupted_read));
            fs::remove_file(repo.0.join(".oms/plan/autopilot-run.json")).unwrap();

            let small = call(&Reads::new("complete"), &["prompt", "--max-bytes", "512"]);
            assert_eq!(small.0, 0, "{}", message(&small));
            assert!(small.1.len() <= 512);
            assert!(
                std::str::from_utf8(&small.1)
                    .unwrap()
                    .contains("truncated:")
            );
            let small_body = std::str::from_utf8(&small.1).unwrap();
            assert!(small_body.contains("p=producer loss,r=render loss"));
            for section in [
                "Goal",
                "State",
                "Decisions",
                "Open findings",
                "Verified / Not verified",
                "Next step",
                "Pointers",
            ] {
                let state = flags(small_body, section);
                assert_ne!(state, "[e]", "trimmed content is not empty: {small_body}");
            }
            assert!(flags(small_body, "Verified / Not verified").contains('r'));
            for (mode, gap, absent) in [
                (
                    "unavailable",
                    "native producer unavailable",
                    "receipt_missing",
                ),
                (
                    "malformed",
                    "producer projection unavailable or malformed",
                    "\"passed\"",
                ),
                (
                    "incomplete",
                    "native producer unavailable",
                    "incomplete_probe",
                ),
                (
                    "line_budget",
                    "projection exceeds line budget",
                    "oversize_probe",
                ),
                (
                    "read_budget",
                    "native producer unavailable",
                    "oversize_probe",
                ),
            ] {
                let result = call(&Reads::new(mode), &["prompt"]);
                assert_eq!(result.0, 0, "{mode}: {}", message(&result));
                let body = std::str::from_utf8(&result.1).unwrap();
                assert!(
                    body.contains(gap) && !body.contains(absent),
                    "{mode}: {body}"
                );
                let expected = if mode == "line_budget" { 'r' } else { 'u' };
                assert!(
                    flags(body, "Verified / Not verified").contains(expected),
                    "{mode}: {body}"
                );
                let tiny = call(&Reads::new(mode), &["prompt", "--max-bytes", "512"]);
                assert_eq!(tiny.0, 0, "{mode}: {}", message(&tiny));
                assert!(tiny.1.len() <= 512);
                assert!(
                    flags(
                        std::str::from_utf8(&tiny.1).unwrap(),
                        "Verified / Not verified"
                    )
                    .contains(expected)
                );
            }
            let collections = call(
                &Reads::new("collections"),
                &["prompt", "--max-bytes", "65536"],
            );
            assert_eq!(collections.0, 0, "{}", message(&collections));
            let body = std::str::from_utf8(&collections.1).unwrap();
            for name in ["criterion", "action", "decision", "finding"] {
                for n in 0..6 {
                    assert!(body.contains(&format!("\"{name}-{n:02}\"")), "{name}: {n}");
                }
                assert!(!body.contains(&format!("\"{name}-06\"")), "{name}");
            }
            assert!(body.contains("truncated: 81 line(s) omitted; withheld: 0 line(s)"));
            assert_eq!(flags(body, "Verified / Not verified"), "[pr]");
            assert_eq!(flags(body, "Open findings"), "[r]");
            let ordinary_budget = call(&Reads::new("collections"), &["prompt"]);
            assert_eq!(ordinary_budget.0, 0, "{}", message(&ordinary_budget));
            let body = std::str::from_utf8(&ordinary_budget.1).unwrap();
            assert!(body.len() <= 4096);
            for name in ["criterion-00", "action-00", "decision-00", "finding-00"] {
                assert!(body.contains(name), "default budget hid {name}: {body}");
            }
            for row in body.lines().filter_map(|line| line.strip_prefix("- {")) {
                let (record, _) = row.rsplit_once(" [src: ").unwrap();
                V::parse(format!("{{{record}").as_bytes()).unwrap();
            }

            let blocked = call(&Reads::new("blocked_only"), &["prompt"]);
            assert_eq!(blocked.0, 0, "{}", message(&blocked));
            let body = std::str::from_utf8(&blocked.1).unwrap();
            for n in 0..6 {
                assert!(body.contains(&format!("blocked-{n:02}")), "{body}");
            }
            assert!(body.contains("dependencies-unfinished"));
            assert!(!body.contains("blocked-06") && !body.contains("done-0"));
            assert!(body.contains("truncated: 7 line(s) omitted; withheld: 0 line(s)"));

            let private = ["/ho", "me/private/handoff"].concat();
            let saved = call(
                &Reads::new("sensitive"),
                &["prompt", "--output", ".oms/handoffs/safe.md"],
            );
            assert_eq!(saved.0, 0, "{}", message(&saved));
            let body = fs::read_to_string(repo.0.join(".oms/handoffs/safe.md")).unwrap();
            assert!(body.contains("withheld: this source matched") && !body.contains(&private));
            assert!(flags(&body, "Verified / Not verified").contains('w'));
            assert!(body.contains("finding_1") && body.contains("plan goal: Plan continuity"));
            let tiny = call(&Reads::new("sensitive"), &["prompt", "--max-bytes", "512"]);
            assert_eq!(tiny.0, 0, "{}", message(&tiny));
            assert!(tiny.1.len() <= 512);
            assert!(
                flags(
                    std::str::from_utf8(&tiny.1).unwrap(),
                    "Verified / Not verified"
                )
                .contains('w')
            );

            repo.write(
                ".oms/plan/tasks.json",
                obj([
                    ("schema", 3usize.into()),
                    ("goal", "Withheld plan".into()),
                    ("accept", "".into()),
                    ("tasks", obj([])),
                    ("local_path", private.as_str().into()),
                ])
                .json(false)
                .unwrap(),
            );
            let result = call(&Reads::new("complete"), &["prompt"]);
            assert_eq!(result.0, 0, "{}", message(&result));
            let body = std::str::from_utf8(&result.1).unwrap();
            assert!(body.contains("withheld: this source matched"));
            assert!(
                body.contains("task goal: Resume the recorded λ goal.")
                    && body.contains("decision_1")
            );
            assert!(!body.contains(&private) && !body.contains("Withheld plan"));
            repo.write(
                ".oms/plan/tasks.json",
                br#"{"schema":3,"goal":"Plan continuity","accept":"","tasks":{}}"#,
            );

            let checks = [
                (
                    format!("{packet}\n## Notes\n{private}\n"),
                    "withheld: this source matched",
                ),
                ("x".repeat(65537), "task source unavailable"),
            ];
            for (packet, gap) in checks {
                repo.write(".oms/task/current.md", packet);
                let result = call(&Reads::new("complete"), &["prompt"]);
                assert_eq!(result.0, 0, "{}", message(&result));
                let body = std::str::from_utf8(&result.1).unwrap();
                assert!(body.contains(gap) && !body.contains(&private));
                assert!(body.contains("plan goal: Plan continuity") && body.contains("decision_1"));
                for section in ["Goal", "Decisions"] {
                    assert!(
                        flags(body, section).contains(if gap.starts_with("withheld") {
                            'w'
                        } else {
                            'u'
                        })
                    );
                }
            }
            let empty_packet = "# Task\n- task_id: handoff-fixture\n- status: active\n\n## Goal\nContinue\n\n## Decisions\n";
            repo.write(".oms/task/current.md", empty_packet);
            let empty = call(&Reads::new("empty"), &["prompt"]);
            assert_eq!(empty.0, 0, "{}", message(&empty));
            let body = std::str::from_utf8(&empty.1).unwrap();
            for section in [
                "Decisions",
                "Open findings",
                "Verified / Not verified",
                "Next step",
            ] {
                assert_eq!(flags(body, section), "[e]", "{body}");
            }
            let empty = call(&Reads::new("empty"), &["prompt", "--max-bytes", "512"]);
            assert_eq!(empty.0, 0, "{}", message(&empty));
            assert!(empty.1.len() <= 512);
            let body = std::str::from_utf8(&empty.1).unwrap();
            for section in [
                "Decisions",
                "Open findings",
                "Verified / Not verified",
                "Next step",
            ] {
                assert_eq!(flags(body, section), "[e]", "{body}");
            }
            assert!(!body.contains("no records returned"));
            let retained = body.lines().filter(|line| line.starts_with("- ")).count();
            assert!(
                body.contains(&format!(
                    "truncated: {} line(s) omitted; withheld: 0 line(s)",
                    4 - retained
                )),
                "{body}"
            );

            // Removing producer-loss explanations does not count their records twice.
            let omitted = call(
                &Reads::new("omitted_one"),
                &["prompt", "--max-bytes", "512"],
            );
            assert_eq!(omitted.0, 0, "{}", message(&omitted));
            let body = std::str::from_utf8(&omitted.1).unwrap();
            assert!(!body.contains("no records returned"));
            let retained = body.lines().filter(|line| line.starts_with("- ")).count();
            assert!(
                body.contains(&format!(
                    "truncated: {} line(s) omitted; withheld: 0 line(s)",
                    7 - retained
                )),
                "{body}"
            );
            for section in ["Decisions", "Verified / Not verified", "Next step"] {
                assert_eq!(flags(body, section), "[p]", "{body}");
            }

            repo.write(
                ".oms/task/current.md",
                empty_packet.replace("Continue", " \t "),
            );
            repo.write(
                ".oms/plan/tasks.json",
                br#"{"schema":3,"goal":" \t ","accept":"","tasks":{}}"#,
            );
            let whitespace = call(&Reads::new("empty"), &["prompt", "--max-bytes", "512"]);
            assert_eq!(whitespace.0, 0, "{}", message(&whitespace));
            let body = std::str::from_utf8(&whitespace.1).unwrap();
            assert_eq!(flags(body, "Goal"), "[e]", "{body}");
            repo.write(".oms/task/current.md", empty_packet);
            repo.write(
                ".oms/plan/tasks.json",
                br#"{"schema":3,"goal":"Plan continuity","accept":"","tasks":{}}"#,
            );
            let omitted = call(
                &Reads::new("omitted_only"),
                &["prompt", "--max-bytes", "512"],
            );
            assert_eq!(omitted.0, 0, "{}", message(&omitted));
            assert!(omitted.1.len() <= 512);
            let body = std::str::from_utf8(&omitted.1).unwrap();
            for section in ["Decisions", "Verified / Not verified", "Next step"] {
                assert!(flags(body, section).contains('p'), "{body}");
                assert!(!flags(body, section).contains('e'), "{body}");
            }
            for (decisions, count) in [
                (
                    (0..9)
                        .map(|n| format!("- 2026-11-20T00:00:00Z [main] decision {n}\n"))
                        .collect::<String>(),
                    3,
                ),
                (
                    format!("- 2026-11-20T00:00:00Z [main] {}\n", "λ".repeat(2200)),
                    1,
                ),
            ] {
                repo.write(".oms/task/current.md", format!("{empty_packet}{decisions}"));
                let result = call(&Reads::new("empty"), &["prompt", "--max-bytes", "65536"]);
                assert_eq!(result.0, 0, "{}", message(&result));
                let body = std::str::from_utf8(&result.1).unwrap();
                assert_eq!(flags(body, "Decisions"), "[p]");
                assert!(
                    body.contains(&format!("truncated: {count} line(s) omitted")),
                    "{body}"
                );
                repo.write(
                    ".oms/task/current.md",
                    format!("{empty_packet}{decisions}")
                        .replace("status: active", "status: closed"),
                );
                let closed = call(&Reads::new("empty"), &["prompt"]);
                assert_eq!(closed.0, 0, "{}", message(&closed));
                let body = std::str::from_utf8(&closed.1).unwrap();
                assert_eq!(flags(body, "Decisions"), "[e]");
                assert!(body.contains("truncated: 0 line(s) omitted"), "{body}");
            }
            repo.write(
                ".oms/task/current.md",
                empty_packet.replace("Continue", &"λ".repeat(301)),
            );
            let shortened = call(&Reads::new("empty"), &["prompt"]);
            assert_eq!(shortened.0, 0, "{}", message(&shortened));
            assert!(flags(std::str::from_utf8(&shortened.1).unwrap(), "Goal").contains('r'));
            repo.write(".oms/task/current.md", packet);
            let interrupted = call(&Reads::new("interrupted"), &["prompt"]);
            assert_eq!(interrupted.0, 130, "{}", message(&interrupted));
            assert!(interrupted.1.is_empty());
        },
    );
}

#[test]
fn registered_pi_session_resumes_with_owning_home() {
    account_isolated(
        "registered_pi_session_resumes_with_owning_home",
        &[("PI_CODING_AGENT_SESSION_DIR".into(), "elsewhere".into())],
        |home| {
            let repo = account_repo();
            let session_dir = home.join("acc/pi1/sessions");
            fs::create_dir_all(&session_dir).unwrap();
            fs::write(
                session_dir.join("one.jsonl"),
                format!(
                    "{{\"type\":\"session\",\"id\":\"sess-1\",\"cwd\":\"{}\"}}\n",
                    repo.0.display()
                ),
            )
            .unwrap();
            fs::write(home.join("bin/pi"), concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = --help ]; then\n",
                "  echo --session\n",
                "  exit\n",
                "fi\n",
                "test -n \"$OMS_PROVIDER_CALL_ID\" || exit 77\n",
                "test -s .oms/usage/receipts.jsonl || exit 78\n",
                "grep -F \"$OMS_PROVIDER_CALL_ID\" .oms/artifacts/panel-dispatch/reservations.json >/dev/null || exit 79\n",
                "printf '%s' \"$OMS_PROVIDER_CALL_ID\" > \"$HOME/resume-call\"\n",
                "printf '%s|%s' \"$PI_CODING_AGENT_DIR\" \"${PI_CODING_AGENT_SESSION_DIR-unset}\" > \"$HOME/resumed\"\n",
            )).unwrap();
            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            let resume_args = [
                "resume",
                "--provider",
                "pi",
                "--session",
                "sess-1",
                "--apply",
            ];
            let preview = run(&repo, source, "session", &resume_args[..5]);
            assert_eq!(preview.0, 0, "{}", message(&preview));
            assert!(
                oms_command_telemetry::store::usage::load(&repo.0)
                    .unwrap()
                    .calls
                    .is_empty()
            );
            assert!(!home.join("resumed").exists());
            let out = run(&repo, source, "session", &resume_args);
            assert_eq!(out.0, 0, "{}", message(&out));
            let projection = oms_command_telemetry::store::usage::load(&repo.0).unwrap();
            assert_eq!(
                projection.calls.len(),
                1,
                "native resume must register a call"
            );
            let receipt = &projection.calls[0].receipt;
            assert_eq!(receipt.identity.provider, "pi");
            assert_eq!(receipt.identity.account.as_deref(), Some("p1"));
            assert_eq!(
                receipt.phase,
                oms_command_telemetry::store::usage::Phase::Unknown
            );
            assert_eq!(receipt.cleanup_complete, Some(true));
            let intents = oms_command_telemetry::store::physical::load(&repo.0, &STOP).unwrap();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].identity, receipt.identity);
            assert_eq!(intents[0].usage_ordinal, Some(receipt.ordinal));
            assert_eq!(
                fs::read_to_string(home.join("resume-call")).unwrap(),
                receipt.identity.call_id,
                "the actual process sees its pre-published call identity"
            );
            assert_eq!(
                fs::read_to_string(home.join("resumed")).unwrap(),
                format!("{}|unset", home.join("acc/pi1").display())
            );
            fs::remove_file(home.join("resumed")).unwrap();
            let registry = repo
                .0
                .join(oms_command_telemetry::store::dispatch::REGISTRY);
            let registered = fs::read(&registry).unwrap();
            fs::write(&registry, b"{corrupt").unwrap();
            let refused = run(&repo, source, "session", &resume_args);
            assert_ne!(refused.0, 0);
            assert!(
                !home.join("resumed").exists(),
                "failed registration must prevent resume"
            );
            assert_eq!(fs::read(&registry).unwrap(), b"{corrupt");
            assert_eq!(
                oms_command_telemetry::store::usage::load(&repo.0)
                    .unwrap()
                    .calls
                    .len(),
                1
            );
            fs::write(&registry, registered).unwrap();
            fs::set_permissions(home.join("acc/pi1"), fs::Permissions::from_mode(0o777)).unwrap();
            let bad = run(&repo, source, "session", &resume_args);
            assert_ne!(bad.0, 0);
            assert!(message(&bad).contains("unavailable"), "{}", message(&bad));
            assert!(!home.join("resumed").exists());
            fs::set_permissions(home.join("acc/pi1"), fs::Permissions::from_mode(0o700)).unwrap();

            struct Unacknowledged;
            impl Executor for Unacknowledged {
                fn execute(
                    &self,
                    request: Invocation,
                    stop: &AtomicBool,
                ) -> oms_command_peer::Result<Completed> {
                    if request.argv[0] == "pi"
                        && request.argv.get(1).is_none_or(|arg| arg != "--help")
                    {
                        return Err(oms_command_peer::Error::new(
                            125,
                            "fixture: no launch acknowledgement",
                        ));
                    }
                    Native.execute(request, stop)
                }
            }
            let missing = oms_command_peer::run_with_executor(
                "session",
                &args(&resume_args),
                &repo.0,
                source,
                &STOP,
                &Unacknowledged,
            );
            assert_eq!(missing.0, 125, "{}", message(&missing));
            assert!(!home.join("resumed").exists());
            let after = oms_command_telemetry::store::usage::load(&repo.0).unwrap();
            assert_eq!(after.calls.len(), 2);
            let unacknowledged = &after.calls[1].receipt;
            assert_eq!(
                unacknowledged.phase,
                oms_command_telemetry::store::usage::Phase::Unknown
            );
            assert_eq!(
                unacknowledged.cleanup_complete, None,
                "exit 125 is no cleanup acknowledgement"
            );

            // Corrupt account registry fails closed instead of falling back to an inherited home.
            let default_sessions = home.join(".pi/agent/sessions");
            fs::create_dir_all(&default_sessions).unwrap();
            fs::write(
                default_sessions.join("def.jsonl"),
                format!(
                    "{{\"type\":\"session\",\"id\":\"sess-def\",\"cwd\":\"{}\"}}\n",
                    repo.0.display()
                ),
            )
            .unwrap();
            fs::write(
                home.join(".config/oh-my-setting/provider-accounts.json"),
                "{corrupt",
            )
            .unwrap();
            let corrupt = run(
                &repo,
                source,
                "session",
                &[
                    "resume",
                    "--provider",
                    "pi",
                    "--session",
                    "sess-def",
                    "--apply",
                ],
            );
            assert_ne!(corrupt.0, 0);
            assert!(!home.join("resumed").exists());
        },
    );
}

#[test]
fn retries_reselect_unpinned_accounts() {
    account_isolated("retries_reselect_unpinned_accounts", &[], |home| {
        let repo = account_repo();
        fs::write(home.join("bin/claude"), "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\" >> \"$HOME/retries\"\necho 'server unavailable' >&2\nexit 1\n").unwrap();
        let _ = account_delegate(&repo, &["--to", "claude", "--repair", "1"]);
        let calls = fs::read_to_string(home.join("retries")).unwrap();
        let calls: Vec<_> = calls.lines().collect();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].ends_with("acc/overflow-a1"), "{calls:?}");
        assert!(calls[1].ends_with("acc/work"), "{calls:?}");
        let repo = account_repo();
        fs::write(home.join("bin/claude"), "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\" >> \"$HOME/capacity-calls\"\necho 'rate limit' >&2\nexit 1\n").unwrap();
        let _ = account_delegate(&repo, &["--to", "claude", "--fallback-model", "other"]);
        let calls = fs::read_to_string(home.join("capacity-calls")).unwrap();
        let calls: Vec<_> = calls.lines().collect();
        assert_eq!(calls.len(), 4, "{calls:?}");
        assert!(calls[2].ends_with("acc/overflow-a1"), "{calls:?}");

        let dirty_repo = account_repo();
        fs::write(home.join("bin/claude"), "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\" >> \"$HOME/dirty-capacity-calls\"\ntouch dirty.txt\necho 'rate limit' >&2\nexit 1\n").unwrap();
        let dirty_out = run_with_source(
            "peer-delegate",
            &args(&[
                "--repo",
                dirty_repo.0.to_str().unwrap(),
                "--to",
                "claude",
                "--fallback-model",
                "other",
                "--model",
                "fixture",
                "--no-verify",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "Write probe",
            ]),
            &dirty_repo.0,
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &STOP,
        );
        assert_ne!(dirty_out.0, 0);
        let dirty_calls = fs::read_to_string(home.join("dirty-capacity-calls")).unwrap();
        assert_eq!(dirty_calls.lines().count(), 1, "{dirty_calls}");

        // Overwriting a pre-existing untracked file keeps porcelain status unchanged yet must refuse retries.
        let edited_repo = account_repo();
        fs::write(edited_repo.0.join("scratch.txt"), "before\n").unwrap();
        fs::write(home.join("bin/claude"), "#!/bin/sh\nprintf '%s\\n' \"$CLAUDE_CONFIG_DIR\" >> \"$HOME/edited-capacity-calls\"\nprintf 'after\\n' > scratch.txt\necho 'rate limit' >&2\nexit 1\n").unwrap();
        let edited_out = run_with_source(
            "peer-delegate",
            &args(&[
                "--repo",
                edited_repo.0.to_str().unwrap(),
                "--to",
                "claude",
                "--fallback-model",
                "other",
                "--model",
                "fixture",
                "--no-verify",
                "--no-task",
                "--no-memory",
                "--no-ml-context",
                "Write probe",
            ]),
            &edited_repo.0,
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")),
            &STOP,
        );
        assert_ne!(edited_out.0, 0);
        let edited_calls = fs::read_to_string(home.join("edited-capacity-calls")).unwrap();
        assert_eq!(edited_calls.lines().count(), 1, "{edited_calls}");

        // Implicit ~/.claude fallback registry collides cleanly if Codex already claims ~/.claude.
        let cfg = home.join(".config/oh-my-setting");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::set_permissions(home.join(".claude"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            cfg.join("provider-accounts.json"),
            format!(
                "{{\"schema\":1,\"providers\":{{\"codex\":{{\"accounts\":[{{\"id\":\"steal\",\"home\":\"{}/.claude\"}}]}}}}}}",
                home.display()
            ),
        )
        .unwrap();
        fs::write(
            cfg.join("provider-accounts.claude.json"),
            format!(
                "{{\"schema\":1,\"provider\":\"claude\",\"accounts\":[{{\"id\":\"work\",\"home\":\"{}/acc/work\"}}]}}",
                home.display()
            ),
        )
        .unwrap();
        let collide = account_delegate(&repo, &["--to", "claude", "--account", "work"]);
        assert_ne!(collide.0, 0);
        assert!(
            message(&collide).contains("duplicate"),
            "{}",
            message(&collide)
        );
    });
}

#[test]
fn delegate_attaches_active_task_packet_only_for_matching_task_or_explicit_flag() {
    provider_isolated(
        "delegate_attaches_active_task_packet_only_for_matching_task_or_explicit_flag",
        "override",
        || {
            let repo = Repo::new();
            repo.write("tracked.txt", "base\n");
            repo.git(&["add", "tracked.txt"]);
            repo.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "base",
            ]);
            let marker = "ACTIVE_TASK_PACKET_MARKER_459";
            let active_packet = format!(
                "# Active Agent Task\n\n- created: 2026-11-20T00:00:00Z\n- updated: 2026-11-20T00:00:00Z\n- task_id: active-task-459\n- status: active\n\n## Goal\n{marker}\n"
            );
            repo.write(".oms/task/current.md", &active_packet);

            let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            let delegate_prompt = |extra: &[&str]| -> String {
                let mut argv = vec![
                    "--repo",
                    repo.0.to_str().unwrap(),
                    "--to",
                    "codex",
                    "--model",
                    "fixture",
                    "--dry-run",
                    "--no-memory",
                    "--no-ml-context",
                ];
                argv.extend_from_slice(extra);
                argv.push("Unrelated write brief");
                let out = run_with_source("peer-delegate", &args(&argv), &repo.0, source, &STOP);
                assert_eq!(out.0, 0, "{extra:?}: {}", String::from_utf8_lossy(&out.2));
                let artifact = String::from_utf8_lossy(&out.1)
                    .lines()
                    .next()
                    .expect("artifact path")
                    .to_owned();
                fs::read_to_string(&artifact).expect("delegate dry-run artifact")
            };

            let unrelated = delegate_prompt(&[]);
            assert!(
                !unrelated.contains(marker),
                "unrelated write delegation attached active task packet:\n{unrelated}"
            );

            let different_id = delegate_prompt(&["--task-id", "other-task"]);
            assert!(
                !different_id.contains(marker),
                "mismatched --task-id attached active task packet:\n{different_id}"
            );

            let matching_id = delegate_prompt(&["--task-id", "active-task-459"]);
            assert!(
                matching_id.contains(marker),
                "matching --task-id missed active task packet:\n{matching_id}"
            );

            let explicit_task = delegate_prompt(&["--task"]);
            assert!(
                explicit_task.contains(marker),
                "explicit --task missed active task packet:\n{explicit_task}"
            );

            let suppressed_matching =
                delegate_prompt(&["--task-id", "active-task-459", "--no-task"]);
            assert!(
                !suppressed_matching.contains(marker),
                "--no-task failed to suppress matching task packet:\n{suppressed_matching}"
            );

            let suppressed_explicit = delegate_prompt(&["--task", "--no-task"]);
            assert!(
                !suppressed_explicit.contains(marker),
                "--no-task failed to suppress explicit --task:\n{suppressed_explicit}"
            );

            let read_matching = delegate_prompt(&["--read-only", "--task-id", "active-task-459"]);
            assert!(
                !read_matching.contains(marker),
                "read-only delegation attached task packet without --task:\n{read_matching}"
            );

            let read_explicit = delegate_prompt(&["--read-only", "--task"]);
            assert!(
                read_explicit.contains(marker),
                "read-only delegation with --task missed task packet:\n{read_explicit}"
            );

            let closed_packet = active_packet.replace("- status: active", "- status: closed");
            repo.write(".oms/task/current.md", &closed_packet);
            let closed_matching = delegate_prompt(&["--task-id", "active-task-459"]);
            assert!(
                !closed_matching.contains(marker),
                "closed task packet attached by default:\n{closed_matching}"
            );
            let closed_explicit = delegate_prompt(&["--task"]);
            assert!(
                closed_explicit.contains(marker),
                "explicit --task should still attach packet:\n{closed_explicit}"
            );

            repo.write(
                ".oms/task/current.md",
                format!("# Active Agent Task\n\n## Goal\n{marker}\n"),
            );
            let legacy_matching = delegate_prompt(&["--task-id", "legacy"]);
            assert!(
                !legacy_matching.contains(marker),
                "task packet without task_id matched by default:\n{legacy_matching}"
            );

            let help = run_with_source("peer-delegate", &args(&["--help"]), &repo.0, source, &STOP);
            assert_eq!(help.0, 0);
            let help_text = String::from_utf8_lossy(&help.1);
            assert!(
                !help_text.contains("(default for writes)."),
                "stale --task help text:\n{help_text}"
            );
            assert!(
                help_text.contains("when --task-id matches the active task's id"),
                "help text should describe task binding default:\n{help_text}"
            );
        },
    );
}
