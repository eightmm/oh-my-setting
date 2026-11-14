#![forbid(unsafe_code)]

use oms_command_lifecycle::{
    IncomingPhase, Output, Services, UnavailableServices, run_with_services,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[derive(Default)]
struct NativeServices(RefCell<Vec<String>>);
impl NativeServices {
    fn record(&self, name: &str) -> Output {
        self.0.borrow_mut().push(name.into());
        (0, b"{}\n".to_vec(), Vec::new())
    }
}
impl Services for NativeServices {
    fn install_extras(
        &self,
        phase: IncomingPhase,
        _: &Path,
        _: &[OsString],
        _: &AtomicBool,
    ) -> Output {
        self.record(&format!("extras:{phase:?}"))
    }
    fn hook_protection(&self, _: &Path, _: &AtomicBool) -> Output {
        self.record("protection")
    }
    fn apply_project_template(&self, _: &str, _: &Path, _: &Path, _: &AtomicBool) -> Output {
        self.record("template")
    }
    fn provider_hook(
        &self,
        _: &str,
        _: &[u8],
        _: &Path,
        _: &Path,
        _: bool,
        _: &AtomicBool,
    ) -> Output {
        self.record("hook")
    }
    fn incoming_lifecycle(
        &self,
        phase: IncomingPhase,
        _: &Path,
        _: &str,
        _: &[u8],
        _: &[OsString],
        _: &AtomicBool,
    ) -> Output {
        self.record(&format!("incoming:{phase:?}"))
    }
    fn tick_maintenance(&self, _: &Path, _: &Path, _: &AtomicBool) -> Output {
        self.record("maintenance");
        (
            0,
            serde_json::to_vec(&json!({
                "journal_rc":0,"ci_rc":0,"plugin_rc":0,"plan_retire_rc":0,
                "artifact_resolve_rc":0,"artifact_supersede_rc":0,"threads_closed":[],
                "tasks_closed":0,"plans_retired":0,"artifacts_resolved":0,
                "artifacts_superseded":0,"failures_retired":0
            }))
            .unwrap(),
            Vec::new(),
        )
    }
    fn tick_schedule(&self, args: &[OsString], _: &Path, _: &Path, _: &AtomicBool) -> Output {
        assert_eq!(args.first().unwrap(), "schedule");
        self.record("schedule")
    }
    fn refresh_catalogs(&self, _: bool, _: &Path, _: &AtomicBool) -> Output {
        self.record("catalogs")
    }
    fn garbage_collect(&self, _: &Path, _: &AtomicBool) -> Output {
        self.record("gc")
    }
    fn wake_idle_main(&self, _: bool, _: &Path, _: &AtomicBool) -> Output {
        self.record("wake");
        (0, b"{\"wake\":0,\"wakes\":[]}\n".to_vec(), Vec::new())
    }
}
fn call(tool: &str, args: &[&str], source: &Path, host: &dyn Services) -> Output {
    run_with_services(
        tool,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        source,
        source,
        &AtomicBool::new(false),
        host,
    )
}
#[track_caller]
fn success(out: Output) -> Vec<u8> {
    assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
    out.1
}
fn write(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn native_only(value: &Value) {
    let raw = value.to_string();
    for retired in ["python", ".py", ".sh", "scripts/oms"] {
        assert!(
            !raw.contains(retired),
            "retired runtime in configuration: {raw}"
        );
    }
}

#[test]
fn native_configuration_and_services() {
    if std::env::var_os("OMS_LIFECYCLE_FIXTURE").is_none() {
        let root = std::env::temp_dir().join(format!("oms-native-test-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Fixture(root);
        let git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(git.status.success());
        fs::create_dir(fixture.0.join("bin")).unwrap();
        symlink(
            String::from_utf8(git.stdout).unwrap().trim(),
            fixture.0.join("bin/git"),
        )
        .unwrap();
        let codex = fixture.0.join("bin/codex");
        fs::write(&codex, br##"#!/bin/sh
case "$*" in
  'plugin list --json') printf '%s\n' '{"installed":[{"pluginId":"oh-my-setting@oh-my-setting-local","installed":true}]}' ;;
  'plugin marketplace list') printf 'oh-my-setting-local %s/source\n' "$OMS_LIFECYCLE_FIXTURE" ;;
esac
"##).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        for dir in ["home", "source", "config", "data", "state"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(fixture.0.join(dir))
                .unwrap();
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_configuration_and_services",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", fixture.0.join("bin"))
            .env("HOME", fixture.0.join("home"))
            .env("XDG_CONFIG_HOME", fixture.0.join("config"))
            .env("XDG_DATA_HOME", fixture.0.join("data"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .env("TMPDIR", &fixture.0)
            .env("OMS_LIFECYCLE_FIXTURE", &fixture.0)
            .env(
                "OH_MY_SETTING_AUTO_UPDATE_CRON_FILE",
                fixture.0.join("cron"),
            )
            .env("OMS_TICK_WAKE", "1")
            .env("OMS_TICK_GC", "1")
            .output()
            .unwrap();
        if !out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let root = PathBuf::from(std::env::var_os("OMS_LIFECYCLE_FIXTURE").unwrap());
    let source = root.join("source");
    let home = root.join("home");
    let host = NativeServices::default();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(source.join("VERSION"), "1.0.0\n").unwrap();
    fs::create_dir(source.join("rules")).unwrap();
    fs::write(source.join("rules/global-AGENTS.md"), "managed rules\n").unwrap();
    write(&source.join("skills.manifest.json"), &json!({"skills":[]}));
    write(
        &source.join(".agents/plugins/marketplace.json"),
        &json!({"name":"oh-my-setting-local"}),
    );
    write(
        &source.join("plugins/oh-my-setting/.codex-plugin/plugin.json"),
        &json!({"name":"oh-my-setting","version":"1.0.0","hooks":"./hooks.json"}),
    );
    let template = source.join("plugins/oh-my-setting/hooks.json");
    write(
        &template,
        &json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"bash \"${CLAUDE_PLUGIN_ROOT:-.}/scripts/harness-hook.sh\" skill-router","timeout":5}]}]}}),
    );
    success(call("install-codex-plugin", &[], &source, &host));
    let cache = home.join(".codex/plugins/cache/oh-my-setting-local/oh-my-setting/1.0.0");
    native_only(&read(&cache.join("hooks.json")));
    assert!(!cache.join("scripts").exists());
    let payload = fs::read(cache.join("hooks.json")).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    assert_eq!(fs::read(cache.join("hooks.json")).unwrap(), payload);
    // The native inventory does not read the legacy executable template.
    fs::remove_file(&template).unwrap();
    success(call("install-codex-plugin", &[], &source, &host));
    assert_eq!(fs::read(cache.join("hooks.json")).unwrap(), payload);
    let hooks_file = cache.join("hooks.json");
    write(&hooks_file, &json!({"hooks":{},"foreign":true}));
    assert_ne!(call("install-codex-plugin", &[], &source, &host).0, 0);
    assert_eq!(read(&hooks_file)["foreign"], true);
    fs::write(&hooks_file, &payload).unwrap();
    let manifest: Value = serde_json::from_slice(&success(call(
        "install-claude-hooks",
        &["--print-expected"],
        &source,
        &host,
    )))
    .unwrap();
    native_only(&manifest);
    assert_eq!(manifest["surfaces"].as_array().unwrap().len(), 13);
    assert_eq!(manifest["hooks_schema"], 12);
    let settings = home.join(".claude/settings.json");
    let legacy = |script: &str| {
        format!(
            "bash '{}' bash '{}'",
            source.join("scripts/lib/python-entry.sh").display(),
            source.join("scripts").join(script).display()
        )
    };
    write(
        &settings,
        &json!({"custom":true,"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[
        {"type":"command","command":legacy("command-guard-hook.sh"),"onFailure":"block"},
        {"type":"command","command":"custom-hook"}
    ]}]},"statusLine":{"type":"command","command":format!("python3 '{}'",source.join("scripts/claude-statusline.py").display())}}),
    );
    success(call("install-claude-hooks", &[], &source, &host));
    let installed = read(&settings);
    native_only(&installed);
    assert_eq!(installed["custom"], true);
    assert!(installed.to_string().contains("custom-hook"));
    assert!(installed.to_string().contains("onFailure"));
    success(call("install-claude-hooks", &[], &source, &host));
    assert_eq!(read(&settings), installed);
    let report: Value = serde_json::from_slice(&success(call(
        "doctor",
        &["--surfaces", "--json"],
        &source,
        &host,
    )))
    .unwrap();
    assert!(!report.to_string().contains("missing or drifted"));
    success(call("install-claude-hooks", &["--remove"], &source, &host));
    let removed = read(&settings);
    assert!(removed.to_string().contains("custom-hook"));
    assert!(removed.get("statusLine").is_none());
    write(&settings, &json!({"hooks":[]}));
    assert_ne!(call("install-claude-hooks", &[], &source, &host).0, 0);
    assert_eq!(read(&settings), json!({"hooks":[]}));
    fs::remove_file(&settings).unwrap();
    fs::create_dir(source.join("rust")).unwrap();
    fs::write(source.join("rust/Cargo.toml"), "[workspace]\n").unwrap();
    let account = home.join("account");
    fs::DirBuilder::new().mode(0o700).create(&account).unwrap();
    let account_settings = account.join("settings.json");
    let side = account.join(".oms-claude-hooks.json");
    let identity = fs::metadata(&account).unwrap();
    let effects = root.join("config/oh-my-setting/provider-account-effects.json");
    write(
        &effects,
        &json!({"schema":1,"accounts":[{"provider":"claude","home":account,
        "identity":{"st_dev":identity.dev(),"st_ino":identity.ino()},"settings":account_settings,
        "hooks_receipt":side,"source_root":source}]}),
    );
    let account_args = [
        "--settings",
        account_settings.to_str().unwrap(),
        "--ownership-receipt",
        side.to_str().unwrap(),
    ];
    let account_safe = account.ancestors().all(|path| {
        let m = fs::metadata(path).unwrap();
        [0, rustix::process::geteuid().as_raw()].contains(&m.uid())
            && (m.mode() & 0o022 == 0 || m.mode() & 0o1000 != 0)
    });
    if account_safe {
        success(call("install-claude-hooks", &account_args, &source, &host));
        native_only(&read(&account_settings));
        let account_installed = read(&account_settings);
        success(call("install-claude-hooks", &account_args, &source, &host));
        assert_eq!(read(&account_settings), account_installed);
        let mut remove = account_args.to_vec();
        remove.push("--remove");
        success(call("install-claude-hooks", &remove, &source, &host));
        assert!(read(&account_settings).get("statusLine").is_none());
    } else {
        let out = call("install-claude-hooks", &account_args, &source, &host);
        assert_ne!(out.0, 0);
        assert!(String::from_utf8_lossy(&out.2).contains("unsafe account ancestor"));
        assert!(!account_settings.exists());
        eprintln!(
            "account round trip skipped: sandbox ancestors have unmapped ownership; refusal verified"
        );
    }
    fs::remove_file(&effects).unwrap();
    success(call(
        "install-hooks",
        &["--quick", "--check-protection"],
        &source,
        &host,
    ));
    let push = fs::read_to_string(source.join(".git/hooks/pre-push")).unwrap();
    assert!(push.contains(" pre-push-check"));
    assert!(!push.contains(".sh"));
    success(call("install-hooks", &[], &source, &host));
    assert!(
        fs::read_to_string(source.join(".git/hooks/pre-push"))
            .unwrap()
            .contains("--full")
    );
    assert_ne!(
        call(
            "install-hooks",
            &["--quick", "--check-protection"],
            &source,
            &UnavailableServices
        )
        .0,
        0
    );
    success(call(
        "install-autoupdate",
        &["--method", "cron"],
        &source,
        &host,
    ));
    let cron = fs::read_to_string(root.join("cron")).unwrap();
    assert!(cron.contains(" auto-update apply"));
    assert!(!cron.contains(".sh"));
    success(call("uninstall-autoupdate", &[], &source, &host));
    write(
        &home.join(".claude.json"),
        &json!({"custom":true,"mcpServers":{"oh-my-setting":{"command":"python3","args":[source.join("scripts/oms-mcp-server.py")]}}}),
    );
    success(call("install-mcp", &["--remove"], &source, &host));
    assert_eq!(read(&home.join(".claude.json"))["custom"], true);
    assert!(
        read(&home.join(".claude.json"))["mcpServers"]
            .get("oh-my-setting")
            .is_none()
    );
    write(
        &source.join("config/capability-profiles.json"),
        &json!({"schema":1,"profiles":{"core":{
            "required":["python3","git"],"any_of":[],"optional":[],"managed_tools":[]
        }}}),
    );
    let tools: Value =
        serde_json::from_slice(&success(call("install-tools", &["--plan"], &source, &host)))
            .unwrap();
    assert_eq!(tools["missing"], json!([]));
    assert_eq!(tools["native_runtime"], true);
    // Missing native authority fails before any install or MCP publication.
    assert_ne!(call("install-mcp", &[], &source, &host).0, 0);
    let plan: Value =
        serde_json::from_slice(&success(call("install", &["--plan"], &source, &host))).unwrap();
    native_only(&plan);
    assert_ne!(call("install", &[], &source, &host).0, 0);
    success(call("tick", &["schedule", "--list"], &source, &host));
    success(call("tick", &["refresh-catalogs"], &source, &host));
    fs::create_dir(source.join(".oms")).unwrap();
    // A real sweep reaches the native maintenance, catalog, GC and wake callbacks.
    let sweep = call(
        "tick",
        &["run", "--repo", source.to_str().unwrap(), "--json"],
        &source,
        &host,
    );
    let body: Value = serde_json::from_slice(&sweep.1).unwrap();
    assert_eq!(body["sweeps"][0]["tasks_closed"], 0);
    for method in [
        "protection",
        "schedule",
        "catalogs",
        "maintenance",
        "gc",
        "wake",
    ] {
        assert!(
            host.0.borrow().iter().any(|s| s == method),
            "missing {method}"
        );
    }
    assert_ne!(
        call(
            "tick",
            &["run", "--repo", source.to_str().unwrap()],
            &source,
            &UnavailableServices
        )
        .0,
        0
    );
    // Uninstall migrates a legacy owned interpreter link without requiring its producer.
    fs::create_dir_all(home.join(".local/bin")).unwrap();
    symlink(
        root.join("data/oh-my-setting/python-runtime/bin/python3"),
        home.join(".local/bin/python3"),
    )
    .unwrap();
    success(call("uninstall", &["--yes"], &source, &host));
    assert!(fs::symlink_metadata(home.join(".local/bin/python3")).is_err());
    symlink(
        "../../../data/oh-my-setting/python-runtime/bin/python3",
        home.join(".local/bin/python3"),
    )
    .unwrap();
    success(call("unlink", &[], &source, &host));
    assert!(fs::symlink_metadata(home.join(".local/bin/python3")).is_err());
    symlink("foreign-runtime", home.join(".local/bin/python3")).unwrap();
    success(call("unlink", &[], &source, &host));
    assert_eq!(
        fs::read_link(home.join(".local/bin/python3")).unwrap(),
        PathBuf::from("foreign-runtime")
    );
    // Exercise every unavailable method: none may silently certify native work.
    let missing: &dyn Services = &UnavailableServices;
    let flag = AtomicBool::new(false);
    let operations = [
        missing.install_extras(IncomingPhase::Preflight, &source, &[], &flag),
        missing.hook_protection(&source, &flag),
        missing.apply_project_template("general", &root, &source, &flag),
        missing.provider_hook("skill-router", b"{}", &source, &source, true, &flag),
        missing.incoming_lifecycle(IncomingPhase::Verify, &source, "", b"{}", &[], &flag),
        missing.tick_maintenance(&source, &source, &flag),
        missing.tick_schedule(&[], &source, &source, &flag),
        missing.refresh_catalogs(false, &source, &flag),
        missing.garbage_collect(&source, &flag),
        missing.wake_idle_main(false, &source, &flag),
    ];
    assert!(
        operations
            .iter()
            .all(|out| out.0 == 2 && out.1.is_empty() && !out.2.is_empty())
    );
    // Host implementors keep all ten signatures, including every incoming phase.
    success(host.apply_project_template("general", &root, &source, &flag));
    success(host.provider_hook("skill-router", b"{}", &source, &source, true, &flag));
    for phase in [
        IncomingPhase::Preflight,
        IncomingPhase::Reconcile,
        IncomingPhase::Verify,
        IncomingPhase::RefreshTools,
    ] {
        success(host.install_extras(phase, &source, &[], &flag));
        success(host.incoming_lifecycle(phase, &source, "", b"{}", &[], &flag));
    }
}
