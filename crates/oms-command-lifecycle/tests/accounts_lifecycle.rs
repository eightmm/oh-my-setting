//! Doctor reporting and uninstall cleanup for registered provider accounts.
//!
//! The state is written the way `connect-services --account` records it. A
//! child process with an isolated environment runs each scenario.
use oms_command_lifecycle::{Services, UnavailableServices, run_with_services};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

const ROOT: &str = "OMS_ACCOUNTS_LIFECYCLE_ROOT";

fn isolated(name: &str, body: fn(&Path)) {
    if let Some(root) = std::env::var_os(ROOT) {
        return body(Path::new(&root));
    }
    let root = std::env::temp_dir().join(format!("oms-acct-life-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for dir in ["bin", "home", "source", "config", "data", "state"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(root.join(dir))
            .unwrap();
    }
    let root = root.canonicalize().unwrap();
    let git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    symlink(
        String::from_utf8(git.stdout).unwrap().trim(),
        root.join("bin/git"),
    )
    .unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", root.join("bin"))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("TMPDIR", &root)
        .env(ROOT, &root)
        .output()
        .unwrap();
    let ok = out.status.success();
    if ok {
        fs::remove_dir_all(&root).unwrap();
    }
    assert!(
        ok,
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn call(tool: &str, args: &[&str], source: &Path) -> (i32, Vec<u8>, Vec<u8>) {
    let host: &dyn Services = &UnavailableServices;
    run_with_services(
        tool,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        source,
        source,
        &AtomicBool::new(false),
        host,
    )
}
fn put(path: &Path, value: &Value) {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path.parent().unwrap())
        .unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn get(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn id_of(path: &Path) -> Value {
    let m = fs::symlink_metadata(path).unwrap();
    json!({"st_uid":m.uid(),"st_dev":m.dev(),"st_ino":m.ino()})
}
fn private_dir(path: &Path) {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .unwrap();
}

struct World {
    config: PathBuf,
    source: PathBuf,
    /// provider -> (main home, account home, linked entry names)
    homes: Vec<(&'static str, PathBuf, PathBuf, Vec<&'static str>)>,
}
impl World {
    fn receipt(&self, name: &str) -> PathBuf {
        self.config.join(name)
    }
    fn link(&self, provider: &str, name: &str) -> PathBuf {
        let (_, _, home, _) = self.homes.iter().find(|h| h.0 == provider).unwrap();
        home.join(name)
    }
}

fn world(root: &Path) -> World {
    let source = root.join("source");
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
    fs::create_dir_all(source.join("crates/oms")).unwrap();
    fs::write(source.join("crates/oms/Cargo.toml"), "[package]\n").unwrap();
    put(&source.join("skills.manifest.json"), &json!({"skills":[]}));
    let config = root.join("config/oh-my-setting");
    let mut homes = Vec::new();
    let mut providers = json!({});
    let mut claude_rows = json!([]);
    let mut effects = json!([]);
    let mut codex = json!([]);
    let mut pi = json!([]);
    for (provider, rel, names) in [
        ("codex", ".codex", vec!["AGENTS.md", "skills"]),
        ("pi", ".pi/agent", vec!["AGENTS.md", "skills"]),
        (
            "claude",
            ".claude",
            vec!["CLAUDE.md", "skills", "output-styles/oms-korean.md"],
        ),
    ] {
        let main = root.join("home").join(rel);
        private_dir(&main.join("skills"));
        private_dir(&main.join("output-styles"));
        for file in ["AGENTS.md", "CLAUDE.md", "output-styles/oms-korean.md"] {
            fs::write(main.join(file), "shared\n").unwrap();
        }
        let account = main.join("accounts/work");
        private_dir(&account);
        let mut links = json!({});
        for name in &names {
            if let Some((sub, _)) = name.split_once('/') {
                private_dir(&account.join(sub));
            }
            symlink(main.join(name), account.join(name)).unwrap();
            links[*name] = if provider == "claude" {
                json!({"target":main.join(name),"identity":id_of(&account.join(name))})
            } else {
                json!(main.join(name))
            };
        }
        let info = fs::metadata(&account).unwrap();
        match provider {
            "claude" => {
                claude_rows
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"work","home":account}));
                effects.as_array_mut().unwrap().push(json!({
                    "id":"work","provider":"claude","home":account,
                    "parent":main.join("accounts"),"parent_identity":id_of(&main.join("accounts")),
                    "identity":id_of(&account),"settings":account.join("settings.json"),
                    "hooks_receipt":account.join(".oms-claude-hooks.json"),
                    "source_root":source,"status":"installed","links":links}));
            }
            _ => {
                let rows = if provider == "codex" {
                    &mut codex
                } else {
                    &mut pi
                };
                rows.as_array_mut()
                    .unwrap()
                    .push(json!({"id":"work","home":account,
                    "links":links,"st_dev":info.dev(),"st_ino":info.ino()}));
            }
        }
        providers[provider] = json!({"policy":"ordered","accounts":[
            {"id":"default","home":main},{"id":"work","home":account}]});
        homes.push((provider, main, account, names));
    }
    put(
        &config.join("provider-accounts.json"),
        &json!({"schema":1,"providers":providers}),
    );
    put(
        &config.join("provider-accounts.claude.json"),
        &json!({"schema":1,"provider":"claude","accounts":claude_rows}),
    );
    put(
        &config.join("provider-account-effects.json"),
        &json!({"schema":1,"accounts":effects}),
    );
    put(&config.join("codex-account-links.json"), &codex);
    put(&config.join("pi-account-links.json"), &pi);
    World {
        config,
        source,
        homes,
    }
}

fn account_rows(w: &World) -> Vec<(String, String)> {
    let out = call("doctor", &["--json"], &w.source);
    let report: Value = serde_json::from_slice(&out.1)
        .unwrap_or_else(|_| panic!("doctor json: {}", String::from_utf8_lossy(&out.2)));
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["section"] == "provider accounts")
        .map(|f| {
            (
                f["level"].as_str().unwrap().into(),
                f["message"].as_str().unwrap().into(),
            )
        })
        .collect()
}
fn snapshot(w: &World) -> Vec<Vec<u8>> {
    [
        "provider-accounts.json",
        "provider-accounts.claude.json",
        "provider-account-effects.json",
        "codex-account-links.json",
        "pi-account-links.json",
    ]
    .iter()
    .map(|n| fs::read(w.receipt(n)).unwrap_or_default())
    .collect()
}

#[test]
fn doctor_reports_account_health_without_mutating() {
    isolated("doctor_reports_account_health_without_mutating", |root| {
        let w = world(root);
        let before = snapshot(&w);
        let rows = account_rows(&w);
        assert_eq!(rows.len(), 3, "{rows:?}");
        for provider in ["codex", "pi", "claude"] {
            assert!(
                rows.iter()
                    .any(|(l, m)| l == "ok" && m.starts_with(&format!("{provider}:work"))),
                "{provider}: {rows:?}"
            );
        }
        // Unsafe mode, missing link, missing receipt and missing home each warn.
        let pi_home = &w.homes[1].2;
        fs::set_permissions(pi_home, fs::Permissions::from_mode(0o770)).unwrap();
        fs::remove_file(w.link("claude", "skills")).unwrap();
        fs::remove_file(w.receipt("codex-account-links.json")).unwrap();
        fs::remove_dir_all(&w.homes[0].2).unwrap();
        let rows = account_rows(&w);
        let warn = |provider: &str, needle: &str| {
            assert!(
                rows.iter().any(|(l, m)| l == "warn"
                    && m.starts_with(&format!("{provider}:work"))
                    && m.contains(needle)),
                "{provider} {needle}: {rows:?}"
            );
        };
        warn("pi", "group/other-writable");
        warn("claude", "owned link(s) missing or changed");
        warn("codex", "no link receipt");
        warn("codex", "home is missing");
        // Doctor never mutates the registry or any receipt.
        let after = snapshot(&w);
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1], before[1]);
        assert_eq!(after[2], before[2]);
        assert_eq!(after[4], before[4]);
        assert!(w.link("claude", "CLAUDE.md").exists());
    });
}
#[test]
fn doctor_without_registry_has_no_account_rows_and_flags_bad_registry() {
    isolated(
        "doctor_without_registry_has_no_account_rows_and_flags_bad_registry",
        |root| {
            let w = world(root);
            fs::write(w.receipt("provider-accounts.json"), "{not json").unwrap();
            let rows = account_rows(&w);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, "warn");
            fs::remove_file(w.receipt("provider-accounts.json")).unwrap();
            let claude_fallback = account_rows(&w);
            assert!(
                claude_fallback
                    .iter()
                    .any(|(l, m)| l == "ok" && m.contains("claude:work")),
                "{claude_fallback:?}"
            );
            for orphan in ["codex:work", "pi:work"] {
                assert!(
                    claude_fallback.iter().any(|(l, m)| l == "warn"
                        && m.contains(orphan)
                        && m.contains("orphaned link receipt")),
                    "{orphan}: {claude_fallback:?}"
                );
            }
            fs::remove_file(w.receipt("provider-accounts.claude.json")).unwrap();
            fs::write(w.receipt("codex-account-links.json"), "{bad").unwrap();
            let bad_receipt = account_rows(&w);
            assert!(
                bad_receipt
                    .iter()
                    .any(|(l, m)| l == "warn" && m.contains("receipt unreadable")),
                "{bad_receipt:?}"
            );
            fs::remove_file(w.receipt("provider-account-effects.json")).unwrap();
            fs::remove_file(w.receipt("codex-account-links.json")).unwrap();
            fs::remove_file(w.receipt("pi-account-links.json")).unwrap();
            assert!(account_rows(&w).is_empty());
        },
    );
}

#[test]
fn uninstall_removes_owned_account_links_for_every_provider() {
    isolated(
        "uninstall_removes_owned_account_links_for_every_provider",
        |root| {
            let w = world(root);
            let out = call("uninstall", &["--yes"], &w.source);
            assert_eq!(out.0, 0, "{}", String::from_utf8_lossy(&out.2));
            for (provider, main, account, names) in &w.homes {
                assert!(account.is_dir(), "{provider} home kept");
                for name in names {
                    assert!(
                        fs::symlink_metadata(account.join(name)).is_err(),
                        "{provider} {name} still linked"
                    );
                    assert!(
                        main.join(name).exists(),
                        "{provider} {name} source untouched"
                    );
                }
            }
            assert_eq!(get(&w.receipt("codex-account-links.json")), json!([]));
            assert_eq!(get(&w.receipt("pi-account-links.json")), json!([]));
            assert_eq!(
                get(&w.receipt("provider-account-effects.json"))["accounts"],
                json!([])
            );
            assert_eq!(
                get(&w.receipt("provider-accounts.claude.json"))["accounts"],
                json!([])
            );
            assert_eq!(
                get(&w.receipt("provider-accounts.json"))["providers"],
                json!({})
            );
            let again = call("uninstall", &["--yes"], &w.source);
            assert_eq!(again.0, 0, "{}", String::from_utf8_lossy(&again.2));
        },
    );
}

#[test]
fn uninstall_keeps_everything_when_a_link_is_foreign_or_changed() {
    isolated(
        "uninstall_keeps_everything_when_a_link_is_foreign_or_changed",
        |root| {
            let w = world(root);
            let before = snapshot(&w);
            // Pi: an exact recorded link whose target is outside every owned root.
            let foreign = root.join("foreign");
            private_dir(&foreign);
            let pi_link = w.link("pi", "skills");
            fs::remove_file(&pi_link).unwrap();
            symlink(&foreign, &pi_link).unwrap();
            let mut pi = get(&w.receipt("pi-account-links.json"));
            pi[0]["links"]["skills"] = json!(foreign);
            put(&w.receipt("pi-account-links.json"), &pi);
            let out = call("uninstall", &["--yes"], &w.source);
            assert_ne!(out.0, 0);
            assert!(String::from_utf8_lossy(&out.2).contains("Pi account cleanup incomplete"));
            assert!(fs::symlink_metadata(w.link("codex", "AGENTS.md")).is_ok());
            assert!(fs::symlink_metadata(w.link("claude", "CLAUDE.md")).is_ok());
            // Restore pi; now a replaced Claude link (identity differs) blocks too.
            fs::remove_file(&pi_link).unwrap();
            symlink(w.homes[1].1.join("skills"), &pi_link).unwrap();
            put(
                &w.receipt("pi-account-links.json"),
                &serde_json::from_slice(&before[4]).unwrap(),
            );
            let claude_link = w.link("claude", "CLAUDE.md");
            fs::remove_file(&claude_link).unwrap();
            symlink(w.homes[2].1.join("CLAUDE.md"), &claude_link).unwrap();
            let out = call("uninstall", &["--yes"], &w.source);
            assert_ne!(out.0, 0);
            assert!(
                String::from_utf8_lossy(&out.2).contains("changed Claude account link retained")
            );
            assert!(fs::symlink_metadata(w.link("pi", "AGENTS.md")).is_ok());
            assert_eq!(
                fs::read(w.receipt("provider-account-effects.json")).unwrap(),
                before[2]
            );
        },
    );
}

#[test]
fn doctor_warns_on_receipt_identity_and_schema_drift() {
    isolated(
        "doctor_warns_on_receipt_identity_and_schema_drift",
        |root| {
            let w = world(root);
            let effects = w.receipt("provider-account-effects.json");
            let mut doc = get(&effects);
            doc["accounts"][0]["identity"]["st_ino"] = json!(0);
            put(&effects, &doc);
            assert!(account_rows(&w).iter().any(
                |(level, message)| level == "warn" && message.contains("home identity changed")
            ));

            let mut doc = get(&effects);
            doc["accounts"][0]["links"] = json!("invalid");
            put(&effects, &doc);
            let rows = account_rows(&w);
            assert!(rows.iter().any(|(level, message)| {
                level == "warn" && message.contains("receipt links missing or invalid")
            }));
            assert!(
                !rows
                    .iter()
                    .any(|(level, message)| level == "ok" && message.starts_with("claude:work "))
            );

            doc["accounts"][0].as_object_mut().unwrap().remove("links");
            put(&effects, &doc);
            let rows = account_rows(&w);
            assert!(rows.iter().any(|(level, message)| {
                level == "warn" && message.contains("receipt links missing or invalid")
            }));
            assert!(
                !rows
                    .iter()
                    .any(|(level, message)| level == "ok" && message.starts_with("claude:work "))
            );

            doc["accounts"][0]["links"] =
                json!({"AGENTS.md": {"target": w.source.join("AGENTS.md")}});
            put(&effects, &doc);
            let rows = account_rows(&w);
            assert!(rows.iter().any(|(level, message)| {
                level == "warn" && message.contains("owned link(s) missing or changed")
            }));

            doc["schema"] = json!(2);
            put(&effects, &doc);
            assert!(
                account_rows(&w)
                    .iter()
                    .any(|(level, message)| level == "warn" && message.contains("schema invalid"))
            );
        },
    );
}
