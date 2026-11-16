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
    let mut command = Command::new(std::env::current_exe().unwrap());
    let output = clean_env_child(&mut command)
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
    let mut command = Command::new(std::env::current_exe().unwrap());
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
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../..")),
        &STOP,
        probe,
    )
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
                    let model = if exact && !native_model {
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
        for (option, message) in [
            ("--apply", "patch-admission"),
            ("--graph-context", "context compilation"),
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
    isolated("review_classification_and_validation", || {
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
            "rust/crates/oms-command-room/src/lib.rs",
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
        for (path, risk) in [
            ("hooks/pre-commit", true),
            ("plugins/fixture/hooks.json", true),
            ("scripts/lib/hook_state.py", true),
            ("rust/crates/oms-command-room/src/lib.rs", false),
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
            repo.write(path, "baseline\n");
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
