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
            cleanup_complete: Some(true),
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
                "#!/bin/sh\nprintf 'changed\\n' >> README.md\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}\\n'\n",
            )
            .unwrap();
            fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
            let path_val = format!(
                "{}:{}",
                fake_claude.parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            );
            if std::env::var_os("OMS_PANEL_DISPATCH_INNER").is_none() {
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
                    .env("OMS_PANEL_DISPATCH_INNER", "1")
                    .env("HOME", std::env::var_os("HOME").unwrap_or_default())
                    .env("PATH", &path_val)
                    .env("OMS_PANEL_DISPATCH", "1")
                    .env("OMS_PANEL_MAIN_ATTEMPT", "att_main1234")
                    .env("OMS_PANEL_ROLE", "worker")
                    .env("OMS_PANEL_STRATEGY", "builder")
                    .env("OMS_PANEL_OWNER", "antigravity")
                    .env("OMS_PANEL_PURPOSE", "implement")
                    .env("OMS_PANEL_WORKLOAD", "routine")
                    .env("OMS_PANEL_ACCESS", "write")
                    .env("OMS_PANEL_LABEL", "builder test")
                    .env("OMS_ROOM_ID", "room-1")
                    .env("OMS_ROOM_PARTICIPANT", "call-p1");
                let out = cmd.output().unwrap();
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
            assert_eq!(refs.get("panel_role").text(), Some("worker"));
            assert_eq!(refs.get("panel_strategy").text(), Some("builder"));
            assert_eq!(refs.get("panel_owner").text(), Some("antigravity"));
            assert_eq!(refs.get("panel_purpose").text(), Some("implement"));
            assert_eq!(refs.get("panel_workload").text(), Some("routine"));
            assert_eq!(refs.get("panel_access").text(), Some("write"));
            assert_eq!(refs.get("panel_label").text(), Some("builder test"));
            assert_eq!(refs.get("panel_room_id").text(), Some("room-1"));
            assert_eq!(refs.get("panel_room_participant").text(), Some("call-p1"));

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
