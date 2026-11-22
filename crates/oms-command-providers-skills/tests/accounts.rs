//! Native `connect-services --account` for the env-isolated providers.
//!
//! The command reads HOME and provider env vars, so each test re-executes this
//! binary as a child with an isolated environment and runs its body there.
use oms_command_providers_skills::run_with_source;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

/// Directories with an explicit mode: a group-writable umask must not make
/// the fixture look unsafe to the command.
fn mk(path: impl AsRef<Path>) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(path)
}

const ROOT: &str = "OMS_ACCOUNT_TEST_ROOT";

fn isolated(name: &str, fakes: &[&str], body: fn(&Path)) {
    if let Some(root) = std::env::var_os(ROOT) {
        return body(Path::new(&root));
    }
    let root = std::env::temp_dir().join(format!("oms-accounts-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    mk(root.join("home")).unwrap();
    mk(root.join("bin")).unwrap();
    let root = root.canonicalize().unwrap();
    for fake in fakes {
        let path = root.join("bin").join(fake);
        // Logged in only when the account home holds an `ok` marker.
        fs::write(
            &path,
            "#!/bin/sh\n[ -e \"$CODEX_HOME/ok\" ] || [ -e \"$CLAUDE_CONFIG_DIR/ok\" ]\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(ROOT, &root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("PATH", root.join("bin"))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("OMS_HARNESS_CHILD")
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

fn connect(root: &Path, args: &[&str]) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (code, out, err) = run_with_source(
        "connect-services",
        &args,
        root,
        &source.canonicalize().unwrap(),
        &AtomicBool::new(false),
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}
#[track_caller]
fn ok(root: &Path, args: &[&str]) -> String {
    let out = connect(root, args);
    assert_eq!(out.0, 0, "{args:?}: {}", out.2);
    out.1
}
#[track_caller]
fn refused(root: &Path, args: &[&str], needle: &str) {
    let out = connect(root, args);
    assert_ne!(out.0, 0, "{args:?} must fail");
    assert!(out.2.contains(needle), "{args:?}: {}", out.2);
}
fn rj(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn config(root: &Path) -> PathBuf {
    root.join("home/.config/oh-my-setting")
}
fn identity(path: &Path) -> Value {
    let m = fs::symlink_metadata(path).unwrap();
    json!({"st_uid":m.uid(),"st_dev":m.dev(),"st_ino":m.ino()})
}
/// Main home with the shared entries each provider links; returns (main, names).
fn seed(root: &Path, provider: &str) -> (PathBuf, &'static [&'static str]) {
    let (rel, files, names): (&str, &[&str], &[&str]) = match provider {
        "codex" => (".codex", &["AGENTS.md"], &["AGENTS.md", "skills"]),
        "claude" => (
            ".claude",
            &["CLAUDE.md", "output-styles/oms-korean.md"],
            &["CLAUDE.md", "skills", "output-styles/oms-korean.md"],
        ),
        _ => (".pi/agent", &["AGENTS.md"], &["AGENTS.md", "skills"]),
    };
    let main = root.join("home").join(rel);
    mk(main.join("skills")).unwrap();
    for file in files {
        mk(main.join(file).parent().unwrap()).unwrap();
        fs::write(main.join(file), "shared\n").unwrap();
    }
    (main, names)
}
fn receipt_row(root: &Path, provider: &str, id: &str) -> Option<Value> {
    let (file, nested) = match provider {
        "codex" => ("codex-account-links.json", false),
        "pi" => ("pi-account-links.json", false),
        _ => ("provider-account-effects.json", true),
    };
    let path = config(root).join(file);
    if !path.exists() {
        return None;
    }
    let doc = rj(&path);
    let rows = if nested { &doc["accounts"] } else { &doc };
    rows.as_array()?.iter().find(|r| r["id"] == id).cloned()
}
fn set_recorded(root: &Path, provider: &str, id: &str, name: &str, target: &Path) {
    let file = match provider {
        "codex" => "codex-account-links.json",
        "pi" => "pi-account-links.json",
        _ => "provider-account-effects.json",
    };
    let path = config(root).join(file);
    let mut doc = rj(&path);
    let rows = if provider == "claude" {
        &mut doc["accounts"]
    } else {
        &mut doc
    };
    let row = rows
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == id)
        .unwrap();
    row["links"][name] = if provider == "claude" {
        json!({"target":target,"identity":identity(&row["home"].as_str().map(PathBuf::from).unwrap().join(name))})
    } else {
        json!(target)
    };
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
}
fn registered(root: &Path, provider: &str) -> Vec<String> {
    rj(&config(root).join("provider-accounts.json"))["providers"][provider]["accounts"]
        .as_array()
        .map(|a| a.iter().map(|r| r["id"].as_str().unwrap().into()).collect())
        .unwrap_or_default()
}

#[test]
fn register_repair_remove_each_provider() {
    isolated("register_repair_remove_each_provider", &[], |root| {
        for provider in ["codex", "claude", "pi"] {
            let (main, names) = seed(root, provider);
            let spec = format!("{provider}:work");
            ok(root, &["--account", &spec]);
            let home = main.join("accounts/work");
            assert_eq!(fs::metadata(&home).unwrap().mode() & 0o777, 0o700);
            for name in names {
                assert_eq!(
                    fs::read_link(home.join(name)).unwrap(),
                    main.join(name),
                    "{provider} {name}"
                );
            }
            assert_eq!(registered(root, provider), ["default", "work"]);
            let row = receipt_row(root, provider, "work").expect("ownership receipt");
            assert_eq!(row["home"], json!(home));
            if provider == "claude" {
                // The exact shape uninstall (account_cleanup) validates.
                assert_eq!(row["provider"], "claude");
                assert_eq!(row["status"], "installed");
                assert_eq!(row["parent"], json!(main.join("accounts")));
                assert_eq!(row["parent_identity"], identity(&main.join("accounts")));
                assert_eq!(row["identity"]["st_ino"], identity(&home)["st_ino"]);
                assert_eq!(row["settings"], json!(home.join("settings.json")));
                assert_eq!(
                    row["hooks_receipt"],
                    json!(home.join(".oms-claude-hooks.json"))
                );
                assert!(row["source_root"].as_str().unwrap().starts_with('/'));
                let link = &row["links"]["output-styles/oms-korean.md"];
                assert_eq!(
                    link["identity"],
                    identity(&home.join("output-styles/oms-korean.md"))
                );
                assert_eq!(
                    fs::metadata(home.join("output-styles")).unwrap().mode() & 0o777,
                    0o700
                );
                let registry = rj(&config(root).join("provider-accounts.claude.json"));
                assert_eq!(registry["provider"], "claude");
                assert_eq!(registry["accounts"][0]["id"], "work");
            }
            // Idempotent second registration changes nothing.
            let before = fs::read(config(root).join("provider-accounts.json")).unwrap();
            ok(root, &["--account", &spec]);
            assert_eq!(
                fs::read(config(root).join("provider-accounts.json")).unwrap(),
                before
            );

            // An unowned foreign link is kept by --repair without changing earlier links or receipts; once owned all are repaired.
            let receipt_file = config(root).join(match provider {
                "codex" => "codex-account-links.json",
                "pi" => "pi-account-links.json",
                _ => "provider-account-effects.json",
            });
            let owned_alt = root.join(format!("owned-alt-{provider}"));
            mk(&owned_alt).unwrap();
            fs::remove_file(home.join(names[0])).unwrap();
            symlink(&owned_alt, home.join(names[0])).unwrap();
            set_recorded(root, provider, "work", names[0], &owned_alt);
            let foreign = root.join(format!("foreign-{provider}"));
            mk(&foreign).unwrap();
            fs::remove_file(home.join("skills")).unwrap();
            symlink(&foreign, home.join("skills")).unwrap();
            let before_refusal = fs::read(&receipt_file).unwrap();
            refused(
                root,
                &["--account", &spec, "--repair"],
                "unowned conflicting",
            );
            assert_eq!(fs::read_link(home.join(names[0])).unwrap(), owned_alt);
            assert_eq!(fs::read_link(home.join("skills")).unwrap(), foreign);
            assert_eq!(fs::read(&receipt_file).unwrap(), before_refusal);
            set_recorded(root, provider, "work", "skills", &foreign);
            ok(root, &["--account", &spec, "--repair"]);
            assert_eq!(
                fs::read_link(home.join(names[0])).unwrap(),
                main.join(names[0])
            );
            assert_eq!(
                fs::read_link(home.join("skills")).unwrap(),
                main.join("skills")
            );

            // A changed link blocks removal and keeps every record.
            fs::remove_file(home.join("skills")).unwrap();
            symlink(&foreign, home.join("skills")).unwrap();
            refused(
                root,
                &["--account", &spec, "--remove"],
                "changed account links kept",
            );
            assert_eq!(registered(root, provider), ["default", "work"]);
            assert!(receipt_row(root, provider, "work").is_some());
            for &sib in names.iter().filter(|&&n| n != "skills") {
                assert_eq!(fs::read_link(home.join(sib)).unwrap(), main.join(sib));
            }
            fs::remove_file(home.join("skills")).unwrap();
            symlink(main.join("skills"), home.join("skills")).unwrap();
            set_recorded(root, provider, "work", "skills", &main.join("skills"));

            ok(root, &["--account", &spec, "--remove"]);
            assert!(home.is_dir(), "home is kept");
            for name in names {
                assert!(
                    fs::symlink_metadata(home.join(name)).is_err(),
                    "{name} unlinked"
                );
            }
            assert_eq!(registered(root, provider), ["default"]);
            assert!(receipt_row(root, provider, "work").is_none());
            if provider == "claude" {
                let registry = rj(&config(root).join("provider-accounts.claude.json"));
                assert_eq!(registry["accounts"], json!([]));
            }
            // Removing again is a no-op success.
            ok(root, &["--account", &spec, "--remove"]);
        }
    });
}

#[test]
fn same_id_across_providers_but_never_a_shared_home() {
    isolated(
        "same_id_across_providers_but_never_a_shared_home",
        &[],
        |root| {
            for provider in ["codex", "claude", "pi"] {
                seed(root, provider);
                ok(root, &["--account", &format!("{provider}:work")]);
            }
            let homes: Vec<PathBuf> = ["codex", "claude", "pi"]
                .iter()
                .map(|p| seed(root, p).0.join("accounts/work"))
                .collect();
            assert_eq!(homes.len(), 3);
            for (provider, taken) in [
                ("pi", &homes[0]),
                ("codex", &homes[1]),
                ("claude", &homes[2]),
            ] {
                refused(
                    root,
                    &[
                        "--account",
                        &format!("{provider}:other"),
                        "--home",
                        taken.to_str().unwrap(),
                    ],
                    "canonical home",
                );
            }
            assert_eq!(registered(root, "pi"), ["default", "work"]);
            assert!(!seed(root, "pi").0.join("accounts/other").exists());
            // A same-provider re-registration of its own home stays allowed.
            ok(
                root,
                &["--account", "pi:work", "--home", homes[2].to_str().unwrap()],
            );

            // Re-registering an account present only in provider-accounts.claude.json preserves its custom home.
            let legacy_home = root.join("custom-claude-legacy");
            mk(&legacy_home).unwrap();
            fs::set_permissions(&legacy_home, fs::Permissions::from_mode(0o700)).unwrap();
            let claude_reg = config(root).join("provider-accounts.claude.json");
            let mut claude_doc = rj(&claude_reg);
            claude_doc["accounts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"legacy","home":legacy_home}));
            fs::write(&claude_reg, serde_json::to_vec(&claude_doc).unwrap()).unwrap();
            ok(root, &["--account", "claude:legacy"]);
            assert_eq!(
                rj(&config(root).join("provider-accounts.json"))["providers"]["claude"]["accounts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["id"] == "legacy")
                    .unwrap()["home"],
                json!(legacy_home)
            );
            assert_eq!(
                rj(&claude_reg)["accounts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["id"] == "legacy")
                    .unwrap()["home"],
                json!(legacy_home)
            );

            // Seeding provider-accounts.json from provider-accounts.claude.json refuses
            // before mutating files or receipts if CLAUDE_CONFIG_DIR points to a registered account.
            let mut without_claude = rj(&config(root).join("provider-accounts.json"));
            without_claude["providers"]
                .as_object_mut()
                .unwrap()
                .remove("claude");
            fs::write(
                config(root).join("provider-accounts.json"),
                serde_json::to_vec(&without_claude).unwrap(),
            )
            .unwrap();
            let effects_before =
                fs::read(config(root).join("provider-account-effects.json")).unwrap();
            let claude_before = fs::read(&claude_reg).unwrap();
            let shared_before = fs::read(config(root).join("provider-accounts.json")).unwrap();
            let mig_out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "claude_legacy_migration_env_collision_child",
                    "--nocapture",
                ])
                .env(ROOT, root)
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("home/.config"))
                .env("CLAUDE_CONFIG_DIR", &legacy_home)
                .output()
                .unwrap();
            assert!(
                mig_out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&mig_out.stdout),
                String::from_utf8_lossy(&mig_out.stderr)
            );
            assert!(!root.join("custom-claude-migrated").exists());
            assert_eq!(
                fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                effects_before
            );
            assert_eq!(fs::read(&claude_reg).unwrap(), claude_before);
            assert_eq!(
                fs::read(config(root).join("provider-accounts.json")).unwrap(),
                shared_before
            );
        },
    );
}

#[test]
fn claude_legacy_migration_env_collision_child() {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        return;
    };
    refused(
        &root,
        &[
            "--account",
            "claude:migrated",
            "--home",
            root.join("custom-claude-migrated").to_str().unwrap(),
        ],
        "duplicate provider account id or canonical home",
    );
}

#[test]
fn unproved_providers_fail_closed_without_writes() {
    isolated(
        "unproved_providers_fail_closed_without_writes",
        &[],
        |root| {
            for provider in ["antigravity", "agy", "gemini", ""] {
                for extra in [&[][..], &["--verify"], &["--remove"]] {
                    let spec = format!("{provider}:work");
                    let mut args = vec!["--account", spec.as_str()];
                    args.extend(extra);
                    refused(
                        root,
                        &args,
                        "provider account isolation is unproved and unavailable",
                    );
                }
            }
            assert!(!config(root).exists());
        },
    );
}

#[test]
fn account_id_and_home_edges() {
    isolated("account_id_and_home_edges", &[], |root| {
        seed(root, "pi");
        refused(root, &["--account", "pi:default"], "default is reserved");
        refused(
            root,
            &["--account", "pi:Bad"],
            "invalid provider account id",
        );
        refused(root, &["--account", "pi:"], "invalid provider account id");
        refused(
            root,
            &["--account", "pi:w", "--first", "--last"],
            "choose --first or --last",
        );
        let main = seed(root, "pi").0;
        refused(
            root,
            &["--account", "pi:w", "--home", main.to_str().unwrap()],
            "differ from the main home",
        );
        let long = format!("pi:{}", "a".repeat(65));
        refused(root, &["--account", &long], "invalid provider account id");
        ok(root, &["--account", &format!("pi:{}", "a".repeat(64))]);
        ok(root, &["--account", "pi:first", "--first"]);
        assert_eq!(registered(root, "pi")[0], "first");

        // The same malformed/valid registry corpus is exercised by peer and panel.
        // Connect retains strict schema/null-policy rules and its ownership writer.
        let corpus_root = root.join("registry-corpus");
        for name in ["one", "two", "three"] {
            mk(corpus_root.join(name)).unwrap();
        }
        symlink(corpus_root.join("one"), corpus_root.join("alias")).unwrap();
        let corpus: Value = serde_json::from_str(include_str!(
            "../../oms-context/tests/fixtures/provider-accounts-v1.json"
        ))
        .unwrap();
        let registry = config(root).join("provider-accounts.json");
        for case in corpus.as_array().unwrap() {
            let bytes = case["document"]
                .as_str()
                .unwrap()
                .replace("@ROOT@", corpus_root.to_str().unwrap());
            fs::write(&registry, &bytes).unwrap();
            let out = connect(root, &["--account", "pi:corpus"]);
            let expected = case["connect"].as_bool().unwrap();
            assert_eq!(out.0 == 0, expected, "{}: {}", case["name"], out.2);
            if !expected {
                assert_eq!(fs::read_to_string(&registry).unwrap(), bytes);
            }
        }
        let mut padded = corpus[0]["document"]
            .as_str()
            .unwrap()
            .replace("@ROOT@", corpus_root.to_str().unwrap())
            .into_bytes();
        padded.resize(1024 * 1024 + 1, b' ');
        fs::write(&registry, &padded).unwrap();
        ok(root, &["--account", "pi:corpus"]);
        padded.resize(4 * 1024 * 1024 + 1, b' ');
        fs::write(&registry, &padded).unwrap();
        refused(root, &["--account", "pi:corpus"], "invalid JSON");
        assert_eq!(fs::read(&registry).unwrap(), padded);
    });
}

#[test]
fn verify_each_provider() {
    isolated("verify_each_provider", &["codex", "claude"], |root| {
        let (codex_main, _) = seed(root, "codex");
        ok(root, &["--account", "codex:work"]);
        let out = connect(root, &["--account", "codex:work", "--verify"]);
        assert_eq!(out.0, 1, "{}", out.1);
        assert!(out.1.contains("warn: ") && out.1.contains("login unverified"));
        for home in [codex_main.clone(), codex_main.join("accounts/work")] {
            fs::write(home.join("ok"), "").unwrap();
        }
        let out = ok(root, &["--account", "codex:work", "--verify"]);
        assert!(
            out.contains("ok: ") && out.contains("codex account work logged in"),
            "{out}"
        );
        fs::set_permissions(
            codex_main.join("accounts/work"),
            fs::Permissions::from_mode(0o770),
        )
        .unwrap();
        assert_eq!(connect(root, &["--account", "codex:work", "--verify"]).0, 1);
        fs::set_permissions(
            codex_main.join("accounts/work"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();

        for provider in ["claude", "pi"] {
            let (main, _) = seed(root, provider);
            ok(root, &["--account", &format!("{provider}:work")]);
            for home in [main.clone(), main.join("accounts/work")] {
                let marker = if provider == "pi" { "auth.json" } else { "ok" };
                fs::write(home.join(marker), "").unwrap();
            }
            let out = connect(
                root,
                &["--account", &format!("{provider}:work"), "--verify"],
            );
            assert_eq!(out.0, 2, "{provider}: {:?} {:?}", out.1, out.2);
            assert!(!out.1.contains("logged in"), "{provider}: {}", out.1);
            assert!(
                out.2.contains("cannot be attested"),
                "{provider}: {}",
                out.2
            );
        }
    });
}

#[test]
fn claude_verify_uses_credentials_without_the_binary() {
    isolated(
        "claude_verify_uses_credentials_without_the_binary",
        &[],
        |root| {
            let (main, _) = seed(root, "claude");
            ok(root, &["--account", "claude:work"]);
            for home in [main.clone(), main.join("accounts/work")] {
                fs::write(home.join(".credentials.json"), "").unwrap();
            }
            let out = connect(root, &["--account", "claude:work", "--verify"]);
            assert_eq!(out.0, 2, "{:?} {:?}", out.1, out.2);
            assert!(!out.1.contains("logged in"), "{}", out.1);
            assert!(out.2.contains("cannot be attested"), "{}", out.2);

            let key = format!("ANTHROPIC_{}", "API_KEY");
            let env_out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "claude_verify_env_override_child", "--nocapture"])
                .env(ROOT, root)
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("home/.config"))
                .env(key, "fixture")
                .output()
                .unwrap();
            assert!(
                env_out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&env_out.stdout),
                String::from_utf8_lossy(&env_out.stderr)
            );
        },
    );
}

#[test]
fn claude_verify_env_override_child() {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        return;
    };
    refused(
        &root,
        &["--account", "claude:work", "--verify"],
        "credential environment",
    );
}

#[test]
fn verify_without_registration_fails() {
    isolated("verify_without_registration_fails", &[], |root| {
        refused(
            root,
            &["--account", "pi:work", "--verify"],
            "not registered",
        );
    });
}

#[test]
fn claude_recovers_unrecorded_link_and_rejects_hooks_symlink() {
    isolated(
        "claude_recovers_unrecorded_link_and_rejects_hooks_symlink",
        &[],
        |root| {
            let (main, _) = seed(root, "claude");
            ok(root, &["--account", "claude:work"]);
            let receipt = config(root).join("provider-account-effects.json");
            let mut doc = rj(&receipt);
            doc["accounts"][0]["links"]
                .as_object_mut()
                .unwrap()
                .remove("CLAUDE.md");
            fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
            ok(root, &["--account", "claude:work"]);
            assert!(receipt_row(root, "claude", "work").unwrap()["links"]["CLAUDE.md"].is_null());
            ok(root, &["--account", "claude:work", "--repair"]);
            assert_eq!(
                receipt_row(root, "claude", "work").unwrap()["links"]["CLAUDE.md"]["target"],
                json!(main.join("CLAUDE.md"))
            );
            let account = main.join("accounts/work");
            symlink(main.join("missing"), account.join(".oms-claude-hooks.json")).unwrap();
            refused(
                root,
                &["--account", "claude:work", "--remove"],
                "hooks receipt",
            );
            fs::remove_file(account.join(".oms-claude-hooks.json")).unwrap();
            fs::write(
                account.join(".oms-claude-hooks.json"),
                r#"{"status":"installed"}"#,
            )
            .unwrap();
            fs::remove_file(&receipt).unwrap();
            refused(
                root,
                &["--account", "claude:work", "--remove"],
                "ownership receipt",
            );
            assert_eq!(registered(root, "claude"), ["default", "work"]);
            assert!(account.join("CLAUDE.md").exists());
        },
    );
}

#[test]
fn rejects_other_provider_main_home() {
    isolated("rejects_other_provider_main_home", &[], |root| {
        let (main, _) = seed(root, "pi");
        refused(
            root,
            &["--account", "claude:work", "--home", main.to_str().unwrap()],
            "main home",
        );
    });
}

#[test]
fn preserves_preexisting_links_and_preflights_receipts_and_claude_schema() {
    isolated(
        "preserves_preexisting_links_and_preflights_receipts_and_claude_schema",
        &[],
        |root| {
            // Preexisting matching symlink is not adopted without --repair and survives --remove.
            let (pi_main, _) = seed(root, "pi");
            let pre_home = pi_main.join("accounts/pre");
            mk(&pre_home).unwrap();
            fs::set_permissions(&pre_home, fs::Permissions::from_mode(0o700)).unwrap();
            symlink(pi_main.join("AGENTS.md"), pre_home.join("AGENTS.md")).unwrap();
            ok(root, &["--account", "pi:pre"]);
            assert!(receipt_row(root, "pi", "pre").unwrap()["links"]["AGENTS.md"].is_null());
            ok(root, &["--account", "pi:pre", "--remove"]);
            assert_eq!(
                fs::read_link(pre_home.join("AGENTS.md")).unwrap(),
                pi_main.join("AGENTS.md")
            );

            // Unpublished ownership receipt reserves its canonical home across providers.
            let effects_only = root.join("effects-only");
            mk(&effects_only).unwrap();
            fs::set_permissions(&effects_only, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                config(root).join("provider-account-effects.json"),
                serde_json::to_vec(&json!({
                    "schema": 1,
                    "accounts": [{"id": "orphan", "home": effects_only, "links": {}}]
                }))
                .unwrap(),
            )
            .unwrap();
            refused(
                root,
                &[
                    "--account",
                    "pi:other",
                    "--home",
                    effects_only.to_str().unwrap(),
                ],
                "canonical home",
            );

            // Invalid provider-accounts.claude.json fails before creating account home or effects.
            let (claude_main, _) = seed(root, "claude");
            fs::write(
                config(root).join("provider-accounts.claude.json"),
                r#"{"schema":2,"provider":"claude","accounts":[]}"#,
            )
            .unwrap();
            let before_effects =
                fs::read(config(root).join("provider-account-effects.json")).unwrap();
            refused(root, &["--account", "claude:w2"], "invalid Claude registry");
            assert!(!claude_main.join("accounts/w2").exists());
            assert_eq!(
                fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                before_effects
            );

            // Connect checks policy even for an empty Claude legacy registry.
            for policy in [Value::Null, json!("random")] {
                fs::write(
                    config(root).join("provider-accounts.claude.json"),
                    serde_json::to_vec(
                        &json!({"schema":1,"provider":"claude","policy":policy,"accounts":[]}),
                    )
                    .unwrap(),
                )
                .unwrap();
                refused(root, &["--account", "claude:w2"], "invalid Claude registry");
                assert!(!claude_main.join("accounts/w2").exists());
                assert_eq!(
                    fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                    before_effects
                );
            }

            // Corrupt provider-accounts.claude.json blocks --remove before unlinking or touching shared state.
            fs::write(
                config(root).join("provider-accounts.claude.json"),
                r#"{"schema":1,"provider":"claude","accounts":[]}"#,
            )
            .unwrap();
            ok(root, &["--account", "claude:rem_test"]);
            let rem_home = claude_main.join("accounts/rem_test");
            let shared_before_rem = fs::read(config(root).join("provider-accounts.json")).unwrap();
            fs::write(
                config(root).join("provider-accounts.claude.json"),
                "{corrupt",
            )
            .unwrap();
            refused(
                root,
                &["--account", "claude:rem_test", "--remove"],
                "invalid",
            );
            assert!(rem_home.join("CLAUDE.md").exists());
            assert!(rem_home.join("skills").exists());
            assert_eq!(
                fs::read(config(root).join("provider-accounts.json")).unwrap(),
                shared_before_rem
            );

            // Malformed provider account lists fail before unlinking files or mutating receipts.
            fs::write(
                config(root).join("provider-accounts.claude.json"),
                serde_json::to_vec(&json!({"schema":1,"provider":"claude","accounts":[{"id":"rem_test","home":rem_home}]})).unwrap(),
            )
            .unwrap();
            let effects_before_rem =
                fs::read(config(root).join("provider-account-effects.json")).unwrap();
            let claude_before_rem =
                fs::read(config(root).join("provider-accounts.claude.json")).unwrap();
            fs::write(
                config(root).join("provider-accounts.json"),
                r#"{"schema":1,"providers":{"claude":{"accounts":"invalid"}}}"#,
            )
            .unwrap();
            refused(
                root,
                &["--account", "claude:rem_test", "--remove"],
                "invalid provider accounts",
            );
            assert!(rem_home.join("CLAUDE.md").exists());
            assert!(rem_home.join("skills").exists());
            assert!(rem_home.join("output-styles/oms-korean.md").exists());
            assert_eq!(
                fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                effects_before_rem
            );
            assert_eq!(
                fs::read(config(root).join("provider-accounts.claude.json")).unwrap(),
                claude_before_rem
            );
            for bad_registry in [
                json!({"schema":1,"providers":{"codex":{"accounts":[{"id":"Bad","home":root.join("bad-id-home")}]}}}),
                json!({"schema":1,"providers":{"codex":{"accounts":[]}}}),
            ] {
                fs::write(
                    config(root).join("provider-accounts.json"),
                    serde_json::to_vec(&bad_registry).unwrap(),
                )
                .unwrap();
                refused(
                    root,
                    &["--account", "claude:preflight_new"],
                    "invalid provider accounts",
                );
                assert!(!claude_main.join("accounts/preflight_new").exists());
                assert_eq!(
                    fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                    effects_before_rem
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.claude.json")).unwrap(),
                    claude_before_rem
                );
            }
            for dup_registry in [
                json!({"schema":1,"providers":{"claude":{"accounts":[{"id":"rem_test","home":rem_home},{"id":"rem_test","home":root.join("dup-id-home")}]}}}),
                json!({"schema":1,"providers":{"codex":{"accounts":[{"id":"steal","home":rem_home}]},"claude":{"accounts":[{"id":"rem_test","home":rem_home}]}}}),
            ] {
                fs::write(
                    config(root).join("provider-accounts.json"),
                    serde_json::to_vec(&dup_registry).unwrap(),
                )
                .unwrap();
                refused(
                    root,
                    &["--account", "claude:rem_test", "--remove"],
                    "duplicate provider account id or canonical home",
                );
                assert!(rem_home.join("CLAUDE.md").exists());
                assert!(rem_home.join("skills").exists());
                assert!(rem_home.join("output-styles/oms-korean.md").exists());
                assert_eq!(
                    fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                    effects_before_rem
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.claude.json")).unwrap(),
                    claude_before_rem
                );
            }

            // Ordinary registration refuses nested-directory conflicts before creating earlier links or mutating receipts/registries.
            fs::write(
                config(root).join("provider-accounts.json"),
                &shared_before_rem,
            )
            .unwrap();
            let conflict_home = claude_main.join("accounts/sub_conflict");
            mk(&conflict_home).unwrap();
            fs::set_permissions(&conflict_home, fs::Permissions::from_mode(0o700)).unwrap();
            let effects_snap =
                fs::read(config(root).join("provider-account-effects.json")).unwrap();
            let claude_reg_snap =
                fs::read(config(root).join("provider-accounts.claude.json")).unwrap();
            let shared_reg_snap = fs::read(config(root).join("provider-accounts.json")).unwrap();
            for kind in ["file", "symlink"] {
                let styles = conflict_home.join("output-styles");
                let _ = fs::remove_file(&styles);
                if kind == "file" {
                    fs::write(&styles, "conflict\n").unwrap();
                } else {
                    symlink(root.join("nonexistent-styles"), &styles).unwrap();
                }
                refused(
                    root,
                    &["--account", "claude:sub_conflict"],
                    "cannot open account subdirectory",
                );
                assert!(!conflict_home.join("CLAUDE.md").exists(), "{kind}");
                assert!(!conflict_home.join("skills").exists(), "{kind}");
                assert_eq!(
                    fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                    effects_snap
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.claude.json")).unwrap(),
                    claude_reg_snap
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.json")).unwrap(),
                    shared_reg_snap
                );
            }

            // Malformed links in an existing ownership receipt fail closed before creating any symlink.
            fs::remove_file(rem_home.join("CLAUDE.md")).unwrap();
            let mut effects_bad = rj(&config(root).join("provider-account-effects.json"));
            let rem_entry = effects_bad["accounts"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|r| r["id"] == "rem_test")
                .unwrap();
            rem_entry["links"] = json!("invalid");
            fs::write(
                config(root).join("provider-account-effects.json"),
                serde_json::to_vec(&effects_bad).unwrap(),
            )
            .unwrap();
            let bad_effects_snap =
                fs::read(config(root).join("provider-account-effects.json")).unwrap();
            let bad_claude_snap =
                fs::read(config(root).join("provider-accounts.claude.json")).unwrap();
            let bad_shared_snap = fs::read(config(root).join("provider-accounts.json")).unwrap();
            for extra in [&[][..], &["--repair"][..]] {
                let spec = "claude:rem_test";
                let mut args = vec!["--account", spec];
                args.extend(extra);
                refused(root, &args, "invalid receipt links");
                assert!(!rem_home.join("CLAUDE.md").exists(), "{extra:?}");
                assert_eq!(
                    fs::read(config(root).join("provider-account-effects.json")).unwrap(),
                    bad_effects_snap
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.claude.json")).unwrap(),
                    bad_claude_snap
                );
                assert_eq!(
                    fs::read(config(root).join("provider-accounts.json")).unwrap(),
                    bad_shared_snap
                );
            }
        },
    );
}
