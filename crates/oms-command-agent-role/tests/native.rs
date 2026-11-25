#![cfg(unix)]
use oms_command_agent_role::{owns, run, run_with_source};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::symlink,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

fn call_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("OMS_TEST_CALL_TIMEOUT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .unwrap_or(60),
    )
}

fn wait_for_pid(path: &Path) -> i32 {
    let deadline = Instant::now() + call_timeout();
    loop {
        if let Ok(contents) = fs::read_to_string(path)
            && let Ok(id) = contents.parse::<i32>()
            && id > 0
        {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "fixture never became ready: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oms-native-role-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let f = Self(path.canonicalize().unwrap());
        for dir in ["project", "global", "source/roles", "source/config", "home"] {
            fs::create_dir_all(f.0.join(dir)).unwrap();
        }
        fs::write(
            f.0.join("source/config/panel-routing.json"),
            include_bytes!("data/panel-routing.json"),
        )
        .unwrap();
        fs::write(
            f.0.join("source/config/role-contracts.json"),
            include_bytes!("data/role-contracts.json"),
        )
        .unwrap();
        let contracts: Value =
            serde_json::from_slice(include_bytes!("data/role-contracts.json")).unwrap();
        for role in contracts["roles"].as_object().unwrap().keys() {
            f.write(
                &format!("source/roles/{role}.md"),
                &format!("# Strategy: {role}\n\n## Mandate\n\n- Do {role}.\n"),
            );
        }
        f
    }
    fn write(&self, name: &str, text: &str) {
        let p = self.0.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn call(&self, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        self.call_os(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            false,
            false,
        )
    }
    fn call_os(
        &self,
        args: &[OsString],
        child: bool,
        interrupted: bool,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        self.call_with_store(args, child, interrupted, Some(&self.0.join("global")))
    }
    fn call_with_store(
        &self,
        args: &[OsString],
        child: bool,
        interrupted: bool,
        store: Option<&Path>,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        self.call_env(args, child, interrupted, store, &[])
    }
    fn call_env(
        &self,
        args: &[OsString],
        child: bool,
        interrupted: bool,
        store: Option<&Path>,
        environment: &[(&str, Option<&str>)],
    ) -> (i32, Vec<u8>, Vec<u8>) {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let input = self.0.join(format!("request-{id}.json"));
        let output = self.0.join(format!("response-{id}.json"));
        let bytes: Vec<Vec<u8>> = args.iter().map(|s| s.as_bytes().to_vec()).collect();
        fs::write(
            &input,
            serde_json::to_vec(&json!({"args":bytes,"interrupted":interrupted})).unwrap(),
        )
        .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "abi_child", "--ignored", "--nocapture"])
            .env("OMS_ROLE_TEST_ROOT", &self.0)
            .env("OMS_ROLE_TEST_INPUT", &input)
            .env("OMS_ROLE_TEST_OUTPUT", &output)
            .env("HOME", self.0.join("home"))
            .env_remove("OMS_ROLE_SORT_TEST_STOP")
            .env("OMS_HARNESS_CHILD", if child { "1" } else { "0" });
        if let Some(store) = store {
            command.env("OH_MY_SETTING_ROLES_DIR", store);
        } else {
            command.env_remove("OH_MY_SETTING_ROLES_DIR");
        }
        for (name, value) in environment {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        let mut process = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + call_timeout();
        while process.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = process.kill();
                let _ = process.wait();
                panic!("ABI fixture exceeded OMS_TEST_CALL_TIMEOUT");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let result = process.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let result: (i32, Vec<u8>, Vec<u8>) =
            serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        fs::remove_file(input).unwrap();
        fs::remove_file(output).unwrap();
        result
    }
    fn ok(&self, args: &[&str]) -> Vec<u8> {
        let (code, out, err) = self.call(args);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(err.is_empty());
        out
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
#[ignore]
fn abi_child() {
    let root = PathBuf::from(std::env::var_os("OMS_ROLE_TEST_ROOT").unwrap());
    let input: Value = serde_json::from_slice(
        &fs::read(std::env::var_os("OMS_ROLE_TEST_INPUT").unwrap()).unwrap(),
    )
    .unwrap();
    let args: Vec<_> = input["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            OsString::from_vec(
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_u64().unwrap() as u8)
                    .collect(),
            )
        })
        .collect();
    let interrupted = AtomicBool::new(input["interrupted"].as_bool().unwrap());
    let result = std::thread::scope(|scope| {
        if let Ok(stop) = std::env::var("OMS_ROLE_SORT_TEST_STOP") {
            let root = &root;
            let interrupted = &interrupted;
            scope.spawn(move || {
                let id = wait_for_pid(&root.join("sort.pid"));
                if stop == "interrupt" {
                    interrupted.store(true, Ordering::Relaxed);
                } else {
                    rustix::process::kill_process(
                        rustix::process::Pid::from_raw(id).unwrap(),
                        rustix::process::Signal::TERM,
                    )
                    .unwrap();
                }
            });
        }
        run_with_source(
            &args,
            &root.join("project"),
            &root.join("source"),
            &interrupted,
        )
    });
    fs::write(
        std::env::var_os("OMS_ROLE_TEST_OUTPUT").unwrap(),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

#[test]
fn classifier_help_errors_and_interruption() {
    for args in [
        vec![],
        vec![OsString::from("anything")],
        vec![OsString::from("render")],
    ] {
        assert!(owns(&args));
    }
    let result = run(
        &["--help".into()],
        Path::new("missing"),
        &AtomicBool::new(false),
    );
    assert_eq!(result.0, 0);
    assert!(result.1.starts_with(b"Usage: agent-role.sh"));
    let f = Fixture::new();
    for (args, message) in [
        (vec!["--bad"], "unknown argument: --bad"),
        (vec!["--repo"], "--repo requires a path"),
        (
            vec!["--provider"],
            "--provider requires claude or codex or antigravity",
        ),
        (vec!["--name"], "--name requires a value"),
        (vec!["show"], "show requires --name"),
        (
            vec!["render", "--name", "builder"],
            "render requires --provider claude|codex|antigravity",
        ),
        (
            vec!["render", "--name", "builder", "--provider", "other"],
            "render requires --provider claude|codex|antigravity",
        ),
        (
            vec!["path", "--repo", "absent", "--name", "x"],
            "could not resolve repo for project scope",
        ),
    ] {
        let result = f.call(&args);
        assert_eq!(result.0, 2);
        assert!(result.1.is_empty());
        assert!(String::from_utf8_lossy(&result.2).contains(message));
    }
    for name in ["", "../bad", "a/b", "a b", "é", "x\n", "x\u{7f}"] {
        let result = f.call(&["path", "--name", name]);
        assert_eq!(result.0, 2);
    }
    assert_eq!(
        f.call_os(&["init".into(), "--name".into(), "x".into()], false, true)
            .0,
        130
    );
    assert!(!f.0.join("project/.oms").exists());
    assert_eq!(f.ok(&["--name", "ignored/invalid", "--help"]), result.1);
}

#[test]
fn all_read_modes_precedence_and_missing() {
    let f = Fixture::new();
    assert_eq!(
        f.ok(&["show", "--name", "builder"]),
        fs::read(f.0.join("source/roles/builder.md")).unwrap()
    );
    f.write("global/builder.md", "global\n");
    assert_eq!(f.ok(&["show", "--name", "builder"]), b"global\n");
    f.write("project/.oms/roles/builder.md", "project\n");
    assert_eq!(f.ok(&["show", "--name", "builder"]), b"project\n");
    assert_eq!(
        f.ok(&["--global", "show", "--name", "builder"]),
        b"global\n"
    );
    f.write("global/unique.md", "global only");
    f.write("project/.oms/roles/only-project.md", "project only");
    f.write("project/.oms/roles/ignored.txt", "ignored");
    symlink("builder.md", f.0.join("project/.oms/roles/link.md")).unwrap();
    fs::create_dir(f.0.join("project/.oms/roles/directory.md")).unwrap();
    let list = String::from_utf8(f.ok(&[])).unwrap();
    assert!(list.contains("only-project\n") && list.contains("unique\n"));
    assert_eq!(list.lines().filter(|s| *s == "builder").count(), 1);
    assert!(!list.contains("link\n") && !list.contains("directory\n") && !list.contains("ignored"));
    assert_eq!(f.ok(&["list", "--global"]), list.as_bytes());
    assert_eq!(f.ok(&["show", "--name", "link"]), b"project\n");
    for (args, path) in [
        (
            vec!["resolve", "--name", "builder"],
            "project/.oms/roles/builder.md",
        ),
        (
            vec!["path", "--name", "missing"],
            "project/.oms/roles/missing.md",
        ),
        (
            vec!["path", "--global", "--name", "missing"],
            "global/missing.md",
        ),
    ] {
        let mut expected = f.0.join(path).as_os_str().as_bytes().to_vec();
        expected.push(b'\n');
        assert_eq!(f.ok(&args), expected);
    }
    for action in ["show", "resolve", "render"] {
        let result = f.call(&[action, "--name", "missing", "--provider", "codex"]);
        assert_eq!(result.0, 3);
        assert!(result.1.is_empty());
        assert!(String::from_utf8_lossy(&result.2).contains("no role 'missing'"));
    }
    assert_eq!(f.ok(&["show", "list"]), f.ok(&["list"]));
    fs::rename(
        f.0.join("project/.oms/roles"),
        f.0.join("project/.oms/actual-roles"),
    )
    .unwrap();
    symlink("actual-roles", f.0.join("project/.oms/roles")).unwrap();
    assert!(!String::from_utf8_lossy(&f.ok(&["list"])).contains("only-project\n"));
    assert_eq!(f.ok(&["show", "--name", "builder"]), b"project\n");
}

#[test]
fn init_is_complete_exclusive_and_child_scoped() {
    let f = Fixture::new();
    let output = f.ok(&["init", "--name", "new.role-1_2"]);
    assert!(output.starts_with(b"role: created "));
    let path = f.0.join("project/.oms/roles/new.role-1_2.md");
    let expected = "# Role: new.role-1_2\n\nYou are acting as the \"new.role-1_2\" role for this task.\n\n## Mandate\n\n- Describe what this role does and does not do.\n\n## Rules\n\n- One actionable rule per line.\n\n## Output\n\n- State what the role must produce.\n";
    assert_eq!(fs::read(&path).unwrap(), expected.as_bytes());
    assert_eq!(
        fs::read(f.0.join("project/.oms/.gitignore")).unwrap(),
        b"*\n"
    );
    assert_eq!(f.call(&["init", "--name", "new.role-1_2"]).0, 2);
    assert_eq!(fs::read(&path).unwrap(), expected.as_bytes());
    f.write("project/.oms/.gitignore", "keep\n");
    assert_eq!(
        f.call_os(
            &["init".into(), "--name".into(), "child".into()],
            true,
            false
        )
        .0,
        0
    );
    assert_eq!(
        fs::read(f.0.join("project/.oms/.gitignore")).unwrap(),
        b"keep\n"
    );
    let result = f.call_os(
        &[
            "init".into(),
            "--global".into(),
            "--name".into(),
            "child".into(),
        ],
        true,
        false,
    );
    assert_eq!(result.0, 2);
    assert!(
        String::from_utf8_lossy(&result.2)
            .contains("a harness child cannot mutate parent-owned host or global state")
    );
    assert!(!f.0.join("global/child.md").exists());
    assert_eq!(
        f.call_os(
            &[
                "path".into(),
                "--global".into(),
                "--name".into(),
                "child".into()
            ],
            true,
            false
        )
        .0,
        0
    );
    f.ok(&["init", "--global", "--name", "global-created"]);
    assert!(f.0.join("global/global-created.md").is_file());
    assert_eq!(
        f.call(&["init", "--global", "--name", "global-created"]).0,
        2
    );
    let concurrent = std::thread::scope(|scope| {
        let a = scope.spawn(|| f.call(&["init", "--name", "race"]));
        let b = scope.spawn(|| f.call(&["init", "--name", "race"]));
        (a.join().unwrap(), b.join().unwrap())
    });
    let mut codes = [concurrent.0.0, concurrent.1.0];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    assert!(
        fs::read_to_string(f.0.join("project/.oms/roles/race.md"))
            .unwrap()
            .ends_with("- State what the role must produce.\n")
    );
    assert_eq!(
        fs::read_dir(f.0.join("project/.oms/roles"))
            .unwrap()
            .count(),
        3
    );
}

#[test]
fn init_refuses_links_and_preserves_other_files() {
    let f = Fixture::new();
    f.write("outside", "unchanged");
    fs::create_dir_all(f.0.join("project/.oms/roles")).unwrap();
    for (name, target) in [
        ("existing", f.0.join("outside")),
        ("dangling", f.0.join("absent")),
    ] {
        symlink(target, f.0.join(format!("project/.oms/roles/{name}.md"))).unwrap();
        assert_eq!(f.call(&["init", "--name", name]).0, 2);
    }
    assert_eq!(fs::read(f.0.join("outside")).unwrap(), b"unchanged");
    assert!(!f.0.join("absent").exists());
    fs::remove_dir_all(f.0.join("project/.oms")).unwrap();
    symlink(f.0.join("global"), f.0.join("project/.oms")).unwrap();
    assert_ne!(f.call(&["init", "--name", "blocked"]).0, 0);
    assert!(!f.0.join("global/roles").exists());
}

#[test]
fn render_matches_canonical_goldens_for_both_providers_and_scopes() {
    let f = Fixture::new();
    let cases: Vec<Value> = serde_json::from_slice(include_bytes!("data/render.json")).unwrap();
    for case in cases {
        let role = case["role"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        f.write(&format!("source/roles/{role}.md"), text);
        for provider in ["codex", "claude"] {
            assert_eq!(
                f.ok(&["render", "--name", role, "--provider", provider]),
                case[format!("{provider}-bundled")]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            );
            f.write(&format!("global/{role}.md"), text);
            assert_eq!(
                f.ok(&["render", "--name", role, "--provider", provider, "--global"]),
                case[format!("{provider}-override")]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            );
            fs::remove_file(f.0.join(format!("global/{role}.md"))).unwrap();
            f.write(&format!("project/.oms/roles/{role}.md"), text);
            assert_eq!(
                f.ok(&["render", "--name", role, "--provider", provider]),
                case[format!("{provider}-override")]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            );
            fs::remove_file(f.0.join(format!("project/.oms/roles/{role}.md"))).unwrap();
        }
        let claude_bundled = case["claude-bundled"].as_str().unwrap();
        let desc_line = claude_bundled
            .lines()
            .find(|line| line.starts_with("description: "))
            .unwrap();
        let writable = matches!(role, "builder" | "implementation-worker");
        let antigravity_expected = |allow_write: bool| {
            let mut lines = vec![
                "---".to_string(),
                format!("name: oms-{role}"),
                desc_line.to_string(),
                "mainAgent: false".to_string(),
                "subagent: true".to_string(),
                "model: inherit".to_string(),
                format!(
                    "commandExecutionPolicy: \"{}\"",
                    if allow_write { "sandbox" } else { "off" }
                ),
                "tools:".to_string(),
                "  - view_file".to_string(),
                "  - grep_search".to_string(),
            ];
            if allow_write {
                lines.extend([
                    "  - replace_file_content".to_string(),
                    "  - write_to_file".to_string(),
                    "  - run_command".to_string(),
                ]);
            } else {
                lines.push("inheritMcp: false".into());
            }
            lines.extend([
                "---".to_string(),
                String::new(),
                text.trim_end_matches('\n').to_string() + "\n",
            ]);
            lines.join("\n")
        };
        assert_eq!(
            f.ok(&["render", "--name", role, "--provider", "antigravity"]),
            antigravity_expected(writable).as_bytes()
        );
        f.write(&format!("global/{role}.md"), text);
        assert_eq!(
            f.ok(&[
                "render",
                "--name",
                role,
                "--provider",
                "antigravity",
                "--global"
            ]),
            antigravity_expected(false).as_bytes()
        );
        fs::remove_file(f.0.join(format!("global/{role}.md"))).unwrap();
        f.write(&format!("project/.oms/roles/{role}.md"), text);
        assert_eq!(
            f.ok(&["render", "--name", role, "--provider", "antigravity"]),
            antigravity_expected(false).as_bytes()
        );
        fs::remove_file(f.0.join(format!("project/.oms/roles/{role}.md"))).unwrap();
    }
    assert!(
        !f.0.join("project/.codex").exists()
            && !f.0.join("project/.claude").exists()
            && !f.0.join("project/.agents").exists()
    );
}

#[test]
fn every_contract_role_writer_authority_and_surface() {
    let f = Fixture::new();
    let roles: Value = serde_json::from_slice(include_bytes!("data/role-contracts.json")).unwrap();
    for role in roles["roles"].as_object().unwrap().keys() {
        for provider in ["claude", "codex", "antigravity"] {
            let output =
                String::from_utf8(f.ok(&["render", "--name", role, "--provider", provider]))
                    .unwrap();
            let writable = matches!(
                role.as_str(),
                "worker"
                    | "builder"
                    | "refactorer"
                    | "documenter"
                    | "integrator"
                    | "performance-engineer"
                    | "implementation-worker"
            );
            let marker = match provider {
                "claude" => "Edit, Write, Bash",
                "codex" => "workspace-write",
                _ => "commandExecutionPolicy: \"sandbox\"",
            };
            assert_eq!(output.contains(marker), writable, "{provider}/{role}");
            if provider == "antigravity" {
                assert!(
                    output.contains("mainAgent: false\nsubagent: true\nmodel: inherit\n"),
                    "{role}"
                );
                assert!(
                    output.contains("tools:\n  - view_file\n  - grep_search\n"),
                    "{role}"
                );
                assert_eq!(
                    output.contains("commandExecutionPolicy: \"off\"\n"),
                    !writable,
                    "{role}"
                );
                let frontmatter = output.split("\n---\n").next().unwrap();
                assert_eq!(
                    frontmatter
                        .lines()
                        .find(|line| line.starts_with("inheritMcp:")),
                    (!writable).then_some("inheritMcp: false"),
                    "{role}"
                );
                assert!(!frontmatter.contains("inheritCustomizations:"), "{role}");
                for write_tool in [
                    "  - replace_file_content\n",
                    "  - write_to_file\n",
                    "  - run_command\n",
                ] {
                    assert_eq!(output.contains(write_tool), writable, "{role}/{write_tool}");
                }
                for forbidden in [
                    "effort:",
                    "invoke_subagent",
                    "define_subagent",
                    "manage_subagents",
                    "send_message",
                    "swarm",
                    "run_workflow",
                ] {
                    assert!(!output.contains(forbidden), "{role}/{forbidden}");
                }
            }
        }
    }
    for role in [
        "worker",
        "builder",
        "refactorer",
        "documenter",
        "integrator",
        "performance-engineer",
        "implementation-worker",
    ] {
        f.write(
            &format!("source/roles/{role}.md"),
            "## Mandate\n\nWrite assigned paths.\n",
        );
        for provider in ["claude", "codex", "antigravity"] {
            let marker = match provider {
                "claude" => "Edit, Write, Bash",
                "codex" => "workspace-write",
                _ => "commandExecutionPolicy: \"sandbox\"",
            };
            let args = ["render", "--name", role, "--provider", provider];
            assert!(String::from_utf8_lossy(&f.ok(&args)).contains(marker));
            f.write(&format!("global/{role}.md"), "Override.\n");
            let global_out = String::from_utf8(f.ok(&args)).unwrap();
            assert!(!global_out.contains(marker));
            if provider == "antigravity" {
                assert!(global_out.contains("commandExecutionPolicy: \"off\"\n"));
                assert!(global_out.contains("inheritMcp: false\n"));
                assert!(!global_out.contains("replace_file_content"));
                assert!(!global_out.contains("write_to_file"));
                assert!(!global_out.contains("run_command"));
            }
            f.write(&format!("project/.oms/roles/{role}.md"), "Override.\n");
            let project_out = String::from_utf8(f.ok(&args)).unwrap();
            assert!(!project_out.contains(marker));
            if provider == "antigravity" {
                assert!(project_out.contains("commandExecutionPolicy: \"off\"\n"));
                assert!(project_out.contains("inheritMcp: false\n"));
                assert!(!project_out.contains("replace_file_content"));
                assert!(!project_out.contains("write_to_file"));
                assert!(!project_out.contains("run_command"));
            }
            fs::remove_file(f.0.join(format!("global/{role}.md"))).unwrap();
            fs::remove_file(f.0.join(format!("project/.oms/roles/{role}.md"))).unwrap();
        }
    }
    assert_eq!(f.ok(&["surface"]), include_bytes!("data/surface.txt"));
    assert_eq!(
        f.ok(&["surface", "--json"]),
        include_bytes!("data/surface.json")
    );
    // Surface does not need a data repository or a role name.
    assert_eq!(
        f.ok(&["surface", "--repo", "absent", "--name", "bad/name"]),
        include_bytes!("data/surface.txt")
    );
}

#[test]
fn renderer_controls_newlines_empty_and_policy_errors() {
    let f = Fixture::new();
    for body in [
        "",
        "\n",
        "# Only title\n",
        "# Title\r\n\r\n## Mandate\r\n\r\n- café\r\n",
    ] {
        f.write("source/roles/custom.md", body);
        let out = String::from_utf8(f.ok(&["render", "--name", "custom", "--provider", "codex"]))
            .unwrap();
        assert!(!out.contains('\r'));
        assert!(out.ends_with("\n\"\"\"\n"));
    }
    for ch in (0..=31u8)
        .chain([127])
        .filter(|ch| ![9, 10, 13].contains(ch))
    {
        f.write("source/roles/custom.md", &format!("text{}", char::from(ch)));
        let result = f.call(&["render", "--name", "custom", "--provider", "codex"]);
        assert_eq!(result.0, 1);
        assert!(result.1.is_empty());
        assert!(String::from_utf8_lossy(&result.2).contains("role text holds a control character"));
        assert_eq!(
            f.call(&["render", "--name", "custom", "--provider", "claude"])
                .0,
            0
        );
    }
    f.write("source/roles/custom.md", "text\tallowed\n");
    f.ok(&["render", "--name", "custom", "--provider", "codex"]);
    fs::write(f.0.join("source/roles/custom.md"), [0xff]).unwrap();
    assert_eq!(
        f.call(&["render", "--name", "custom", "--provider", "codex"])
            .0,
        1
    );
    assert_eq!(f.ok(&["show", "--name", "custom"]), [0xff]);
    f.write("source/config/panel-routing.json", "{}");
    let result = f.call(&["render", "--name", "builder", "--provider", "codex"]);
    assert_eq!(result.0, 1);
    assert!(String::from_utf8_lossy(&result.2).contains("unsupported panel routing policy"));
    f.write("source/config/role-contracts.json", "not json");
    assert_eq!(f.call(&["surface"]).0, 1);
}

#[test]
fn git_root_non_git_relative_and_raw_path_bytes() {
    let f = Fixture::new();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(f.0.join("project"))
            .status()
            .unwrap()
            .success()
    );
    fs::create_dir_all(f.0.join("project/nested/deeper")).unwrap();
    let path = f.ok(&["path", "--repo", "nested/deeper", "--name", "."]);
    let mut expected =
        f.0.join("project/.oms/roles/..md")
            .as_os_str()
            .as_bytes()
            .to_vec();
    expected.push(b'\n');
    assert_eq!(path, expected);
    #[cfg(not(target_os = "macos"))]
    {
        let raw = f.0.join(OsString::from_vec(b"raw-\xff".to_vec()));
        fs::create_dir(&raw).unwrap();
        let args = [
            "init".into(),
            "--repo".into(),
            raw.clone().into_os_string(),
            "--name".into(),
            "bytes".into(),
        ];
        let result = f.call_os(&args, false, false);
        assert_eq!(result.0, 0);
        assert!(result.1.contains(&0xff));
        let path_args = [
            "path".into(),
            "--repo".into(),
            raw.clone().into_os_string(),
            "--name".into(),
            "bytes".into(),
        ];
        let result = f.call_os(&path_args, false, false);
        assert_eq!(result.0, 0);
        let mut expected = raw
            .join(".oms/roles/bytes.md")
            .as_os_str()
            .as_bytes()
            .to_vec();
        expected.push(b'\n');
        assert_eq!(result.1, expected);
        assert!(raw.join(".oms/roles/bytes.md").is_file());
    }
}

#[test]
fn routing_defaults_order_and_validation_edges() {
    let f = Fixture::new();
    let baseline: Value =
        serde_json::from_slice(include_bytes!("data/panel-routing.json")).unwrap();
    let apply = |value: &Value| {
        fs::write(
            f.0.join("source/config/panel-routing.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
        f.call(&["render", "--name", "builder", "--provider", "codex"])
    };
    for gap in [0, 100] {
        let mut data = baseline.clone();
        data["tie_band"]["gap"] = json!(gap);
        assert_eq!(apply(&data).0, 0);
    }
    let mut defaults = baseline.clone();
    for key in ["auto_review", "council", "tie_band"] {
        defaults.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(apply(&defaults), apply(&baseline));
    let mut data = baseline.clone();
    data["tiers"]["routine"] = json!([
        {"provider":"claude","model":"claude-opus-5-5","effort":"high"},
        {"provider":"codex","model":"gpt-6-astra","effort":"low"}
    ]);
    let result = apply(&data);
    assert_eq!(result.0, 0);
    assert!(
        String::from_utf8_lossy(&result.1)
            .contains("model = \"gpt-6-astra\"\nmodel_reasoning_effort = \"low\"")
    );
    let claude = f.ok(&["render", "--name", "builder", "--provider", "claude"]);
    assert!(String::from_utf8_lossy(&claude).contains("model: claude-opus-5-5\neffort: high\n"));
    data["tiers"]["routine"].as_array_mut().unwrap().pop();
    let result = apply(&data);
    assert_eq!(result.0, 0);
    assert!(!String::from_utf8_lossy(&result.1).contains("model ="));
    for (provider, model, effort) in [
        ("codex", "gpt-6.2-sol", "xhigh"),
        ("codex", "gpt-6.2-astra", "ultra"),
        ("claude", "claude-opus-5-6", "max"),
        ("claude", "claude-sonnet-5-6", "xhigh"),
    ] {
        let mut data = baseline.clone();
        data["tiers"]["routine"] = json!([{"provider":provider,"model":model,"effort":effort}]);
        assert_eq!(apply(&data).0, 0);
        let rendered = f.ok(&["render", "--name", "builder", "--provider", provider]);
        let rendered = String::from_utf8_lossy(&rendered);
        assert!(
            rendered.contains(model) && rendered.contains(effort),
            "{rendered}"
        );
    }
    for model in ["gemini-pro-orchestrator", "gemini-pro-orchestrator-argon"] {
        let mut data = baseline.clone();
        data["tiers"]["deep"][2] = json!({"provider":"antigravity","model":model,"access":"read"});
        assert_eq!(apply(&data).0, 0);
        data["tiers"]["deep"][2]["effort"] = "high".into();
        assert_ne!(apply(&data).0, 0);
    }
    let mut data = baseline.clone();
    data["tiers"]["deep"][2]["model"] = "gemini-4-hydrogen-xhigh".into();
    data["tiers"]["deep"][2]["effort"] = "xhigh".into();
    assert_eq!(apply(&data).0, 0);
    data["tiers"]["deep"][2]["effort"] = "max".into();
    assert_ne!(apply(&data).0, 0);
    // Antigravity base-form routes and Neon/Hydrogen max/ultra refusals work for both forms.
    for (model, effort, ok) in [
        ("gemini-4-carbon", "high", true),
        ("gemini-4-neon", "xhigh", true),
        ("gemini-4-neon", "max", false),
        ("gemini-4-neon", "ultra", false),
        ("gemini-4-neon-high", "max", false),
        ("gemini-4-neon-high", "ultra", false),
        ("gemini-4-hydrogen", "xhigh", true),
        ("gemini-4-hydrogen", "max", false),
        ("gemini-4-hydrogen", "ultra", false),
        ("gemini-4-high", "high", false),
    ] {
        let mut data = baseline.clone();
        data["tiers"]["deep"][2]["model"] = model.into();
        data["tiers"]["deep"][2]["effort"] = effort.into();
        assert_eq!(apply(&data).0 == 0, ok, "{model} {effort}");
    }
    // Claude latest-model aliases validate and render cleanly, and bundled policy loads.
    for alias in ["opus", "sonnet", "fable", "haiku"] {
        assert_eq!(
            oms_command_agent_role::native_model::route_family("claude", alias),
            Some((alias.to_owned(), vec![u64::MAX]))
        );
        assert!(oms_command_agent_role::native_model::valid_role_model(
            "claude", alias
        ));
        let mut data = baseline.clone();
        data["tiers"]["routine"] = json!([{"provider":"claude","model":alias,"effort":"medium"}]);
        assert_eq!(apply(&data).0, 0, "{alias}");
    }
    assert_eq!(
        oms_command_agent_role::native_model::route_family("antigravity", "gemini-4-carbon"),
        Some(("carbon".to_owned(), vec![4]))
    );
    assert_eq!(
        oms_command_agent_role::native_model::route_family("antigravity", "gemini-4-carbon-high"),
        Some(("carbon-high".to_owned(), vec![4]))
    );
    assert_eq!(
        oms_command_agent_role::native_model::route_family("antigravity", "gemini-4-high"),
        None
    );
    let bundled: Value =
        serde_json::from_slice(include_bytes!("../../../config/panel-routing.json")).unwrap();
    assert_eq!(apply(&bundled).0, 0);
    let mut data = baseline.clone();
    data["tiers"]["routine"][2]
        .as_object_mut()
        .unwrap()
        .remove("effort");
    let default_effort = apply(&data);
    assert_eq!(default_effort.0, 0);
    assert!(!String::from_utf8_lossy(&default_effort.1).contains("model_reasoning_effort ="));
    for (pointer, replacement, message) in [
        ("", json!([]), "unsupported panel routing policy"),
        ("/schema", json!(3), "unsupported panel routing policy"),
        (
            "/auto_review",
            json!(0),
            "panel auto-review setting must be boolean",
        ),
        (
            "/tie_band",
            json!([]),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band",
            json!({"unknown":0}),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band/gap",
            json!(-1),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band/gap",
            json!(101),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band/gap",
            json!(0.0),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band/gap",
            json!("0"),
            "panel tie band limits must be integer percentages",
        ),
        (
            "/tie_band/gap",
            json!(true),
            "panel tie band limits must be integer percentages",
        ),
        ("/council", json!({}), "invalid panel council extra routes"),
        (
            "/council/extra",
            json!(null),
            "invalid panel council extra routes",
        ),
        (
            "/council/extra",
            json!([{"provider":"antigravity","model":"bad/model","effort":"high"}]),
            "invalid panel council extra routes",
        ),
        (
            "/council/extra",
            json!([{"provider":"pi","model":"a/b","effort":"high"},{"provider":"pi","model":"a/c","effort":"high"}]),
            "invalid panel council extra routes",
        ),
        ("/main", json!({}), "invalid panel main routes"),
        (
            "/main/codex/provider",
            json!("claude"),
            "invalid panel route: main/codex",
        ),
        (
            "/main/codex/model",
            json!("unversioned"),
            "invalid panel route: main/codex",
        ),
        (
            "/main/codex/effort",
            json!("not-an-effort"),
            "invalid panel route: main/codex",
        ),
        (
            "/main/claude/model",
            json!("claude-unknown"),
            "invalid panel route: main/claude",
        ),
        ("/tiers", json!({}), "invalid panel tiers"),
        ("/tiers/routine", json!([]), "invalid panel tier: routine"),
        ("/tiers/routine", json!(null), "invalid panel tier: routine"),
        (
            "/tiers/routine",
            json!([{"provider":"codex","model":"gpt-6.1-sol","effort":"high"},{"provider":"codex","model":"gpt-6-astra","effort":"high"}]),
            "duplicate provider in panel tier: routine",
        ),
        (
            "/tiers/deep/2/access",
            json!("write"),
            "invalid panel tier: deep",
        ),
        (
            "/tiers/deep/2/model",
            json!("unversioned"),
            "invalid panel tier: deep",
        ),
        (
            "/tiers/light/0/public_only",
            json!(1),
            "invalid panel tier: light",
        ),
        (
            "/tiers/light/0/access",
            json!("admin"),
            "invalid panel tier: light",
        ),
        (
            "/tiers/light/0/model",
            json!("-a/b"),
            "invalid panel tier: light",
        ),
        (
            "/tiers/light/0/model",
            json!("a/"),
            "invalid panel tier: light",
        ),
        (
            "/tiers/light/0/model",
            json!("a b/c"),
            "invalid panel tier: light",
        ),
        (
            "/tiers/light/0/provider",
            json!("other"),
            "invalid panel tier: light",
        ),
        (
            "/roles",
            json!({}),
            "panel roles must map every role to a tier",
        ),
        (
            "/roles/builder/tier",
            json!("unknown"),
            "invalid panel role: builder",
        ),
    ] {
        let mut data = baseline.clone();
        if let Some(slot) = data.pointer_mut(pointer) {
            *slot = replacement;
        } else {
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            data.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), replacement);
        }
        let result = apply(&data);
        assert_eq!(result.0, 1, "{pointer}");
        assert!(result.1.is_empty());
        assert!(
            String::from_utf8_lossy(&result.2).contains(message),
            "{pointer}: {:?}",
            result.2
        );
    }
    f.write("source/config/panel-routing.json", "not json");
    assert_eq!(
        f.call(&["render", "--name", "builder", "--provider", "codex"])
            .0,
        1
    );
    fs::remove_file(f.0.join("source/config/panel-routing.json")).unwrap();
    assert_eq!(
        f.call(&["render", "--name", "builder", "--provider", "claude"])
            .0,
        1
    );
}

#[test]
fn empty_stores_home_fallback_relative_global_and_raw_arguments() {
    let f = Fixture::new();
    fs::remove_dir_all(f.0.join("source/roles")).unwrap();
    assert!(f.ok(&["list"]).is_empty());
    f.write("source/config/role-contracts.json", "{\"roles\":{}}");
    let value: Value = serde_json::from_slice(&f.ok(&["surface", "--json"])).unwrap();
    assert_eq!(value, json!({"schema":1,"roles":[]}));
    f.write(
        "source/config/role-contracts.json",
        "{\"roles\":{\"résumé\\u007f😀\":{\"read_only\":true,\"writes\":\"none\"}}}",
    );
    assert!(
        String::from_utf8(f.ok(&["surface", "--json"]))
            .unwrap()
            .contains("r\\u00e9sum\\u00e9\\u007f\\ud83d\\ude00")
    );
    for missing in [None, Some(Path::new(""))] {
        let args = [
            "path".into(),
            "--global".into(),
            "--name".into(),
            "home".into(),
        ];
        let result = f.call_with_store(&args, false, false, missing);
        assert_eq!(result.0, 0);
        let mut expected =
            f.0.join("home/.oh-my-setting/local/roles/home.md")
                .as_os_str()
                .as_bytes()
                .to_vec();
        expected.push(b'\n');
        assert_eq!(result.1, expected);
    }
    let args = [
        "init".into(),
        "--global".into(),
        "--name".into(),
        "home".into(),
    ];
    assert_eq!(f.call_with_store(&args, false, false, None).0, 0);
    assert!(
        f.0.join("home/.oh-my-setting/local/roles/home.md")
            .is_file()
    );
    let path_args = [
        "path".into(),
        "--global".into(),
        "--name".into(),
        "relative".into(),
    ];
    assert_eq!(
        f.call_with_store(&path_args, false, false, Some(Path::new("local-roles")))
            .1,
        b"local-roles/relative.md\n"
    );
    let args = [
        "init".into(),
        "--global".into(),
        "--name".into(),
        "relative".into(),
    ];
    assert_eq!(
        f.call_with_store(&args, false, false, Some(Path::new("local-roles")))
            .0,
        0
    );
    assert!(f.0.join("project/local-roles/relative.md").is_file());
    #[cfg(not(target_os = "macos"))]
    {
        fs::create_dir_all(f.0.join("project/.oms/roles")).unwrap();
        let raw = OsString::from_vec(b"raw-\xff.md".to_vec());
        fs::write(f.0.join("project/.oms/roles").join(raw), b"raw").unwrap();
        assert_eq!(f.ok(&["list"]), b"raw-\xff\n");
    }
    let result = f.call_os(&[OsString::from_vec(b"bad-\xff".to_vec())], false, false);
    assert_eq!(result.0, 2);
    assert_eq!(result.2, b"error: unknown argument: bad-\xff\n");
    let args = [
        "path".into(),
        "--name".into(),
        OsString::from_vec(b"raw-\xff".to_vec()),
    ];
    assert_eq!(f.call_os(&args, false, false).0, 2);
}

#[test]
fn init_directory_and_ignore_failure_boundaries() {
    let f = Fixture::new();
    f.write("project/.oms", "ordinary file\n");
    assert_eq!(f.call(&["init", "--name", "blocked"]).0, 1);
    assert_eq!(
        fs::read(f.0.join("project/.oms")).unwrap(),
        b"ordinary file\n"
    );
    fs::remove_file(f.0.join("project/.oms")).unwrap();
    fs::create_dir(f.0.join("project/.oms")).unwrap();
    f.write("outside-ignore", "keep\n");
    symlink(
        f.0.join("outside-ignore"),
        f.0.join("project/.oms/.gitignore"),
    )
    .unwrap();
    f.ok(&["init", "--name", "safe"]);
    assert_eq!(fs::read(f.0.join("outside-ignore")).unwrap(), b"keep\n");
    fs::remove_dir_all(f.0.join("project/.oms/roles")).unwrap();
    symlink(f.0.join("global"), f.0.join("project/.oms/roles")).unwrap();
    assert_eq!(f.call(&["init", "--name", "blocked"]).0, 1);
    assert!(!f.0.join("global/blocked.md").exists());
    fs::remove_dir(f.0.join("global")).unwrap();
    symlink(f.0.join("home"), f.0.join("global")).unwrap();
    assert_eq!(f.call(&["init", "--global", "--name", "blocked"]).0, 1);
    assert!(!f.0.join("home/blocked.md").exists());
}

#[test]
fn list_inherits_locale_and_deduplicates_without_option_injection() {
    let f = Fixture::new();
    fs::remove_dir_all(f.0.join("source/roles")).unwrap();
    fs::create_dir(f.0.join("source/roles")).unwrap();
    let names = ["Z", "a", "é", "--version", "-o", "duplicate"];
    for name in names {
        f.write(&format!("project/.oms/roles/{name}.md"), "project\n");
        f.write(&format!("global/{name}.md"), "global\n");
        f.write(&format!("source/roles/{name}.md"), "bundled\n");
    }
    let locales = Command::new("locale").arg("-a").output();
    let available = locales
        .as_ref()
        .ok()
        .filter(|r| r.status.success())
        .map(|r| String::from_utf8_lossy(&r.stdout).into_owned())
        .unwrap_or_default();
    let is_non_c = |locale: &&str| {
        !matches!(*locale, "C" | "POSIX") && !locale.to_ascii_lowercase().starts_with("c.")
    };
    let is_utf8 = |locale: &&str| {
        let lower = locale.to_ascii_lowercase();
        lower.ends_with(".utf-8") || lower.ends_with(".utf8")
    };
    let non_c = available
        .lines()
        .find(|locale| {
            is_utf8(locale) && (locale.starts_with("en_US") || locale.starts_with("en_GB"))
        })
        .or_else(|| {
            available
                .lines()
                .find(|locale| is_utf8(locale) && is_non_c(locale))
        })
        .or_else(|| {
            available
                .lines()
                .find(|locale| locale.starts_with("en_US") || locale.starts_with("en_GB"))
        })
        .or_else(|| available.lines().find(is_non_c));
    let mut tested = vec!["C"];
    if let Some(locale) = non_c {
        tested.push(locale);
    } else {
        eprintln!("SKIP non-C locale parity: host locale -a exposes no non-C locale");
    }
    for locale in tested {
        for environment in [
            vec![("LC_ALL", Some(locale))],
            vec![
                ("LC_ALL", None),
                ("LC_COLLATE", Some(locale)),
                ("LANG", Some("C")),
            ],
            vec![
                ("LC_ALL", None),
                ("LC_COLLATE", None),
                ("LANG", Some(locale)),
            ],
        ] {
            let mut canonical = Command::new("sort");
            canonical
                .arg("-u")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
            for (name, value) in &environment {
                if let Some(value) = value {
                    canonical.env(name, value);
                } else {
                    canonical.env_remove(name);
                }
            }
            let mut canonical = canonical.spawn().unwrap();
            use std::io::Write;
            canonical
                .stdin
                .take()
                .unwrap()
                .write_all((names.join("\n") + "\n").as_bytes())
                .unwrap();
            let expected = canonical.wait_with_output().unwrap();
            assert!(expected.status.success());
            let result = f.call_env(
                &["list".into()],
                false,
                false,
                Some(&f.0.join("global")),
                &environment,
            );
            assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
            assert_eq!(
                result.1, expected.stdout,
                "locale {locale}, environment {environment:?}"
            );
            assert!(result.2.is_empty());
        }
    }
    assert_eq!(
        fs::read_dir(f.0.join("project/.oms/roles"))
            .unwrap()
            .count(),
        names.len()
    );
}

#[test]
fn list_sort_failure_deadline_interruption_and_reaping() {
    let f = Fixture::new();
    // Exceed pipe capacity so hung/closed stdin exercises the nonblocking pump.
    for index in 0..1000 {
        f.write(
            &format!("project/.oms/roles/{index:06}{}.md", "x".repeat(200)),
            "",
        );
    }
    fs::create_dir(f.0.join("bin")).unwrap();
    let compile = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-name")
        .arg("oms_role_sort_fixture")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/sort_child.rs"))
        .arg("-o")
        .arg(f.0.join("bin/sort"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let mut paths = vec![f.0.join("bin")];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();
    let path = path.to_str().unwrap();
    for (mode, stop, code, message) in [
        ("fail", None, 7, "fixture sort failure"),
        ("stdout-over", None, 1, "stdout exceeds 1048576 bytes"),
        ("stderr-over", None, 1, "stderr exceeds 65536 bytes"),
        ("malformed", None, 1, "malformed result"),
        ("nul", None, 1, "malformed result"),
        ("empty", None, 1, "malformed result"),
        ("closed", None, 1, "malformed result"),
        ("hang", Some("interrupt"), 130, ""),
        ("hang", Some("signal"), 143, "exit 143"),
        ("hang", None, 124, "deadline exceeded"),
    ] {
        fs::remove_file(f.0.join("sort.pid")).ok();
        let result = f.call_env(
            &["list".into()],
            false,
            false,
            Some(&f.0.join("global")),
            &[
                ("PATH", Some(path)),
                ("OMS_ROLE_SORT_FIXTURE_ROOT", Some(f.0.to_str().unwrap())),
                ("OMS_ROLE_SORT_FIXTURE_MODE", Some(mode)),
                ("OMS_ROLE_SORT_TEST_STOP", stop),
            ],
        );
        assert_eq!(
            result.0,
            code,
            "mode {mode}: {}",
            String::from_utf8_lossy(&result.2)
        );
        assert!(result.1.is_empty(), "partial result escaped for {mode}");
        assert!(
            String::from_utf8_lossy(&result.2).contains(message),
            "mode {mode}: {}",
            String::from_utf8_lossy(&result.2)
        );
        if code == 130 {
            assert!(result.2.is_empty());
        }
        let id = fs::read_to_string(f.0.join("sort.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(id).unwrap()),
            Err(rustix::io::Errno::SRCH),
            "sort was not reaped for {mode}"
        );
    }
    for (mode, length) in [("warning", 16), ("stderr-edge", 64 * 1024)] {
        let result = f.call_env(
            &["list".into()],
            false,
            false,
            Some(&f.0.join("global")),
            &[
                ("PATH", Some(path)),
                ("OMS_ROLE_SORT_FIXTURE_ROOT", Some(f.0.to_str().unwrap())),
                ("OMS_ROLE_SORT_FIXTURE_MODE", Some(mode)),
                ("OMS_ROLE_SORT_TEST_STOP", None),
            ],
        );
        assert_eq!(result.0, 0);
        assert!(!result.1.is_empty());
        assert_eq!(result.2.len(), length);
    }
    assert_eq!(
        fs::read_to_string(f.0.join("sort.calls")).unwrap(),
        "-u\n".repeat(12)
    );
    let result = f.call_env(
        &["list".into()],
        false,
        false,
        Some(&f.0.join("global")),
        &[("PATH", Some(""))],
    );
    assert_eq!(result.0, 1);
    assert!(result.1.is_empty());
    assert!(String::from_utf8_lossy(&result.2).contains("sort"));
    assert_eq!(
        fs::read_to_string(f.0.join("sort.calls")).unwrap(),
        "-u\n".repeat(12)
    );
}

#[test]
fn list_input_and_output_limit_edges() {
    let f = Fixture::new();
    fs::remove_dir_all(f.0.join("source/roles")).unwrap();
    assert!(f.ok(&["list"]).is_empty());
    fs::create_dir_all(f.0.join("project/.oms/roles")).unwrap();
    let mut remaining = 1024 * 1024;
    let mut index = 0;
    while remaining > 0 {
        let length = (remaining - 1).min(252);
        assert!(length >= 6);
        let name = format!("{index:06}{}", "x".repeat(length - 6));
        fs::write(
            f.0.join("project/.oms/roles").join(format!("{name}.md")),
            [],
        )
        .unwrap();
        remaining -= length + 1;
        index += 1;
    }
    let result = f.ok(&["list"]);
    assert_eq!(result.len(), 1024 * 1024);
    assert_eq!(result.iter().filter(|b| **b == b'\n').count(), index);
    f.write("project/.oms/roles/extra.md", "");
    let result = f.call(&["list"]);
    assert_eq!(result.0, 1);
    assert!(result.1.is_empty());
    assert!(String::from_utf8_lossy(&result.2).contains("input exceeds 1048576 bytes"));
}

#[test]
fn agy_catalog_row_parses_selector_base_effort_and_label() {
    use oms_command_agent_role::native_model::{
        AgyCatalogRow, agy_catalog_row, agy_catalog_selector,
    };

    assert_eq!(
        agy_catalog_row("gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)"),
        Some(AgyCatalogRow {
            selector: "gemini-4-neon-xhigh",
            base_model: "gemini-4-neon",
            effort: Some("xhigh"),
            label: Some("Gemini 4 Neon (XHigh)"),
        })
    );
    assert_eq!(
        agy_catalog_row("gpt-oss-120b-medium\tGPT-OSS 120B (Medium)"),
        Some(AgyCatalogRow {
            selector: "gpt-oss-120b-medium",
            base_model: "gpt-oss-120b",
            effort: Some("medium"),
            label: Some("GPT-OSS 120B (Medium)"),
        })
    );
    assert_eq!(
        agy_catalog_row("gemini-pro-orchestrator-neon\tOrchestrator (Neon)"),
        Some(AgyCatalogRow {
            selector: "gemini-pro-orchestrator-neon",
            base_model: "gemini-pro-orchestrator-neon",
            effort: None,
            label: Some("Orchestrator (Neon)"),
        })
    );
    assert_eq!(
        agy_catalog_row("gemini-pro-orchestrator\tOrchestrator (Carbon)"),
        Some(AgyCatalogRow {
            selector: "gemini-pro-orchestrator",
            base_model: "gemini-pro-orchestrator",
            effort: None,
            label: Some("Orchestrator (Carbon)"),
        })
    );
    // Label must agree in parenthesised form; bare selectors and non-matching labels keep effort None.
    assert_eq!(
        agy_catalog_row("qwen-3-max\tQwen 3 Max"),
        Some(AgyCatalogRow {
            selector: "qwen-3-max",
            base_model: "qwen-3-max",
            effort: None,
            label: Some("Qwen 3 Max"),
        })
    );
    assert_eq!(
        agy_catalog_row("gemini-4-carbon-high\tGemini 4 Carbon (Medium)"),
        Some(AgyCatalogRow {
            selector: "gemini-4-carbon-high",
            base_model: "gemini-4-carbon-high",
            effort: None,
            label: Some("Gemini 4 Carbon (Medium)"),
        })
    );
    assert_eq!(
        agy_catalog_row("gemini-4-oxygen-low"),
        Some(AgyCatalogRow {
            selector: "gemini-4-oxygen-low",
            base_model: "gemini-4-oxygen-low",
            effort: None,
            label: None,
        })
    );
    assert_eq!(
        agy_catalog_selector("gemini-4-carbon-high\tGemini 4 Carbon (High)"),
        Some("gemini-4-carbon-high")
    );
    for invalid in [
        "",
        "Fetching available models...",
        "gemini-4-bogus-high\twrong\tlabel",
        "gemini-4-malformed-high\t",
        "gemini-4-malformed-high\t   ",
    ] {
        assert_eq!(agy_catalog_row(invalid), None, "{invalid}");
        assert_eq!(agy_catalog_selector(invalid), None, "{invalid}");
    }
}
