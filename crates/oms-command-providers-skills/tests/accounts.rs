//! Native `connect-services --account` for the env-isolated providers.
//!
//! The command reads HOME and provider env vars, so each test re-executes this
//! binary as a child with an isolated environment and runs its body there.
use oms_command_providers_skills::run_with_source;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
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
const BASE: &str = "OMS_ACCOUNT_TEST_BASE";
const WITNESS: &str = "OMS_ACCOUNT_FIXTURE_WITNESS";
const MARKER: &str = ".oms-account-fixture";

/// Where fixture roots live: `pick` runs once in the parent and makes the
/// root, whose parent directory it hands down as BASE; a child only `verify`s
/// that value, since its HOME is the fixture's and recomputing could pick a
/// different parent.
#[derive(Clone, Copy)]
struct Parent {
    pick: fn(&str) -> OwnedRoot,
    verify: fn(&Path) -> Option<String>,
}
const TEMP_PARENT: Parent = Parent {
    pick: |label| {
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        OwnedRoot::create(&tmp, label)
            .unwrap_or_else(|e| panic!("cannot create a fixture root in {}: {e}", tmp.display()))
    },
    verify: |base| {
        (std::env::temp_dir().canonicalize().ok().as_deref() != Some(base))
            .then(|| "not the canonical TMPDIR".into())
    },
};
const PRIVATE_PARENT: Parent = Parent {
    pick: private_root,
    verify: parent_refusal,
};

fn hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .unwrap();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh 0700 `oms-accounts-<pid>-<label>-<hex>` directory bound to its
/// dev/ino: only that exact directory is ever removed.
struct OwnedRoot {
    path: PathBuf,
    id: (u64, u64),
}
impl OwnedRoot {
    /// `Err` only when the mkdir itself fails because `parent` cannot take a
    /// new entry (nothing was made, so another parent may be tried); any
    /// other mkdir error, or a made directory failing the checks, panics.
    /// A failed directory is removed only while it is still the empty one
    /// just made.
    fn create(parent: &Path, label: &str) -> std::io::Result<Self> {
        use std::io::ErrorKind::*;
        use std::os::unix::fs::DirBuilderExt;
        let name = format!("oms-accounts-{}-{label}-{}", std::process::id(), hex(8));
        let path = parent.join(name);
        // Non-recursive: refuses an existing path instead of adopting it.
        if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&path) {
            if matches!(
                e.kind(),
                PermissionDenied
                    | ReadOnlyFilesystem
                    | NotFound
                    | NotADirectory
                    | StorageFull
                    | QuotaExceeded
                    | AlreadyExists
            ) {
                return Err(e);
            }
            panic!("creating fixture root {}: {e}", path.display());
        }
        let uid = rustix::process::getuid().as_raw();
        let m = fs::symlink_metadata(&path).unwrap();
        let id = (m.dev(), m.ino());
        if !(m.file_type().is_dir()
            && m.uid() == uid
            && m.mode() & 0o777 == 0o700
            && path.canonicalize().ok().as_ref() == Some(&path))
        {
            if m.file_type().is_dir()
                && fs::symlink_metadata(&path).is_ok_and(|n| (n.dev(), n.ino()) == id)
            {
                let _ = fs::remove_dir(&path);
            }
            panic!(
                "fixture root {} is not a fresh private directory",
                path.display()
            );
        }
        Ok(Self { path, id })
    }
}
impl Drop for OwnedRoot {
    fn drop(&mut self) {
        let same = fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_dir() && (m.dev(), m.ino()) == self.id);
        if same && self.path.canonicalize().ok().as_ref() == Some(&self.path) {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            eprintln!(
                "fixture root {} was replaced; left in place",
                self.path.display()
            );
        }
    }
}

/// The child half of a fixture: trust the inherited ROOT only when it is the
/// directory a parent fixture made for `owner` under the BASE it handed down
/// (marker holding its witness, the env it set), so a stray
/// `OMS_ACCOUNT_TEST_ROOT` can never aim a body at a real path or real
/// provider config. Read-only.
fn fixture_root(
    parent: Parent,
    owner: &str,
    ppid: Option<i32>,
    claude: Option<&str>,
    xdg: Option<&str>,
) -> PathBuf {
    let raw = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let refuse = |why: &str| -> ! {
        panic!(
            "refusing inherited {ROOT}={}: {why}; only the account fixture parent may set it",
            raw.display()
        )
    };
    let witness = std::env::var_os(WITNESS).unwrap_or_else(|| refuse("no fixture witness"));
    if !raw.is_absolute() || raw.canonicalize().ok().as_ref() != Some(&raw) {
        refuse("not an absolute canonical path");
    }
    let base = PathBuf::from(std::env::var_os(BASE).unwrap_or_else(|| refuse("no fixture base")));
    if !base.is_absolute() || base.canonicalize().ok().as_ref() != Some(&base) {
        refuse("fixture base is not an absolute canonical path");
    }
    if let Some(why) = (parent.verify)(&base) {
        refuse(&format!("fixture base {}: {why}", base.display()));
    }
    let named = raw.file_name().and_then(|n| n.to_str()).and_then(|n| {
        let (pid, rest) = n.strip_prefix("oms-accounts-")?.split_once('-')?;
        Some((pid, rest.strip_prefix(owner)?.strip_prefix('-')?))
    });
    let named = named.is_some_and(|(pid, suffix)| {
        !pid.is_empty()
            && pid.bytes().all(|b| b.is_ascii_digit())
            && ppid.is_none_or(|want| pid == want.to_string())
            && suffix.len() == 16
            && suffix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if raw.parent() != Some(base.as_path()) || !named {
        refuse(&format!(
            "not {}/oms-accounts-<parent pid>-{owner}-<suffix>",
            base.display()
        ));
    }
    let uid = rustix::process::getuid().as_raw();
    let ours = |p: &Path, dir: bool, mask: u32| {
        fs::symlink_metadata(p).is_ok_and(|m| {
            (if dir { m.is_dir() } else { m.is_file() }) && m.uid() == uid && m.mode() & mask == 0
        })
    };
    if !ours(&raw, true, 0o022) {
        refuse("not a directory of ours without group/world write");
    }
    let marker = raw.join(MARKER);
    if !ours(&marker, false, 0o077)
        || fs::read(&marker).ok().as_deref() != Some(witness.as_encoded_bytes())
    {
        refuse("fixture marker missing, not 0600-private, or witness mismatch");
    }
    for (key, want) in [
        ("HOME", raw.join("home")),
        ("XDG_CONFIG_HOME", raw.join(xdg.unwrap_or("home/.config"))),
        ("PATH", raw.join("bin")),
    ] {
        if std::env::var_os(key).as_deref() != Some(want.as_os_str()) {
            refuse(&format!("{key} is not {}", want.display()));
        }
    }
    for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR"] {
        let want = claude
            .filter(|_| key == "CLAUDE_CONFIG_DIR")
            .map(|c| raw.join(c));
        if std::env::var_os(key).map(PathBuf::from) != want {
            refuse(&format!("{key} must be {want:?}"));
        }
    }
    raw
}

fn isolated(name: &str, fakes: &[&str], body: fn(&Path)) {
    isolated_in(TEMP_PARENT, name, fakes, body)
}
fn isolated_in(parent: Parent, name: &str, fakes: &[&str], body: fn(&Path)) {
    if std::env::var_os(ROOT).is_some() {
        let ppid = rustix::process::Pid::as_raw(rustix::process::getppid());
        return body(&fixture_root(parent, name, Some(ppid), None, None));
    }
    let owned = (parent.pick)(name);
    let root = owned.path.clone();
    let base = root.parent().unwrap().to_path_buf();
    mk(root.join("home")).unwrap();
    mk(root.join("bin")).unwrap();
    let witness = hex(16);
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(MARKER))
            .and_then(|mut f| f.write_all(witness.as_bytes()))
            .unwrap();
    }
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
        .env(BASE, &base)
        .env(WITNESS, &witness)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("PATH", root.join("bin"))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("OMS_HARNESS_CHILD")
        .output()
        .unwrap();
    drop(owned);
    let kept = fs::symlink_metadata(&root).is_ok();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!kept, "fixture root {} was not removed", root.display());
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
    if std::env::var_os(ROOT).is_none() {
        return;
    }
    let root = fixture_root(
        TEMP_PARENT,
        "same_id_across_providers_but_never_a_shared_home",
        None,
        Some("custom-claude-legacy"),
        None,
    );
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
            for provider in ["agy", "gemini", ""] {
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
    if std::env::var_os(ROOT).is_none() {
        return;
    }
    let root = fixture_root(
        TEMP_PARENT,
        "claude_verify_uses_credentials_without_the_binary",
        None,
        None,
        None,
    );
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

/// Why `dir` cannot hold fixture roots: not absolute, inside the repository,
/// or an ancestor from `/` that is not a real root- or user-owned directory
/// without group/world write. `dir` is checked as given, so callers
/// canonicalize first and a symlinked ancestor is refused.
fn parent_refusal(dir: &Path) -> Option<String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    if !dir.is_absolute() {
        return Some("not absolute".into());
    }
    if dir.starts_with(&repo) {
        return Some(format!("inside the repository {}", repo.display()));
    }
    let uid = rustix::process::getuid().as_raw();
    let mut chain: Vec<&Path> = dir.ancestors().collect();
    chain.reverse();
    chain
        .into_iter()
        .find(|a| {
            !fs::symlink_metadata(a).is_ok_and(|m| {
                m.file_type().is_dir() && (m.uid() == 0 || m.uid() == uid) && m.mode() & 0o022 == 0
            })
        })
        .map(|a| format!("unsafe ancestor {}", a.display()))
}
/// A root in the first candidate that canonicalizes, passes `parent_refusal`
/// and takes a fresh `OwnedRoot`; panics naming every refused candidate.
fn select_parent(candidates: &[PathBuf], label: &str) -> OwnedRoot {
    let mut refused = Vec::new();
    for candidate in candidates {
        let why = if !candidate.is_absolute() {
            "not absolute".to_string()
        } else {
            match candidate.canonicalize() {
                Err(e) => format!("unresolvable: {e}"),
                Ok(dir) => match parent_refusal(&dir) {
                    None => match OwnedRoot::create(&dir, label) {
                        Ok(root) => return root,
                        Err(e) => format!("cannot create a root: {e}"),
                    },
                    Some(why) => why,
                },
            }
        };
        refused.push(format!("{}: {why}", candidate.display()));
    }
    panic!(
        "antigravity account fixtures need a writable parent outside the repository whose every ancestor is a root- or user-owned directory without group/world write; refused: {}",
        refused.join("; ")
    );
}
/// Antigravity registries and homes refuse a sticky 1777 `/tmp`, so try
/// TMPDIR, XDG_RUNTIME_DIR, `/run/user/<uid>` (also when the variable is
/// unset), then HOME (only ever a parent for a fresh fixture root).
fn private_root(label: &str) -> OwnedRoot {
    let uid = rustix::process::getuid().as_raw();
    let candidates: Vec<PathBuf> = [
        Some(std::env::temp_dir()),
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        Some(PathBuf::from(format!("/run/user/{uid}"))),
        std::env::var_os("HOME").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    .collect();
    let root = select_parent(&candidates, label);
    let base = root.path.parent().unwrap();
    eprintln!("account fixture parent: {}", base.display());
    root
}

#[test]
fn fixture_parent_selection_refuses_shared_symlinked_and_repo_paths() {
    let owned = private_root("parent_selection");
    let at = |name: &str, mode: u32| {
        let dir = owned.path.join(name);
        mk(&dir).unwrap();
        private(&dir, mode);
        dir
    };
    let (good, other) = (at("good", 0o700), at("other", 0o755));
    let sticky = at("sticky", 0o1777);
    // Ancestry-safe but unwritable: selection must fall through, not panic.
    let locked = at("locked", 0o555);
    let locked_before = snapshot(&locked);
    at("group", 0o770);
    let under_group = at("group/inner", 0o700);
    let link = owned.path.join("link");
    symlink(&good, &link).unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parent_of = |candidates: &[PathBuf]| {
        let root = select_parent(candidates, "pick");
        root.path.parent().unwrap().to_path_buf()
    };
    // Root bypasses the 0555 mode, so only a non-root run can see EACCES.
    let as_root = rustix::process::getuid().is_root();
    let picked = parent_of(&[sticky.clone(), locked.clone(), good.clone(), other.clone()]);
    if !as_root {
        assert_eq!(picked, good);
    }
    assert_eq!(snapshot(&locked), locked_before);
    // A symlinked candidate is judged by its physical target ...
    assert_eq!(parent_of(std::slice::from_ref(&link)), good);
    // ... and a symlink inside a path already taken as physical is refused.
    assert_eq!(
        parent_refusal(&link.join("inner")),
        Some(format!("unsafe ancestor {}", link.display()))
    );
    assert_eq!(parent_refusal(&other), None);
    let missing = owned.path.join("missing");
    let mut candidates = vec![
        sticky.clone(),
        under_group,
        repo.clone(),
        missing.clone(),
        PathBuf::from("relative"),
    ];
    let mut wants = vec![
        format!("{}: unsafe ancestor {}", sticky.display(), sticky.display()),
        format!(
            "{}: unsafe ancestor {}",
            owned.path.join("group/inner").display(),
            owned.path.join("group").display()
        ),
        format!("{}: inside the repository", repo.display()),
        format!("{}: unresolvable", missing.display()),
        "relative: not absolute".to_string(),
    ];
    if !as_root {
        candidates.push(locked.clone());
        let denied = "cannot create a root: Permission denied";
        wants.push(format!("{}: {denied}", locked.display()));
    }
    let Err(panic) = std::panic::catch_unwind(|| select_parent(&candidates, "none")) else {
        panic!("every candidate must be refused");
    };
    let message = panic.downcast_ref::<String>().unwrap();
    for want in wants {
        assert!(message.contains(&want), "{want:?} not in {message}");
    }
    assert_eq!(snapshot(&locked), locked_before);
}
fn private(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
/// A profile home with an obviously fake ADC placeholder (never a credential).
fn agy_home(root: &Path, name: &str, profile: bool) -> PathBuf {
    let home = root.join(name);
    mk(&home).unwrap();
    private(&home, 0o700);
    if profile {
        let file = home.join("application_default_credentials.json");
        fs::write(&file, "{\"placeholder\":\"fixture, not a credential\"}\n").unwrap();
        private(&file, 0o600);
    }
    home
}
/// Bytes, modes and inodes of every entry, to prove a home was left alone.
fn snapshot(home: &Path) -> Vec<(OsString, u32, u64, Vec<u8>)> {
    let m = fs::symlink_metadata(home).unwrap();
    let mut rows = vec![(OsString::from("."), m.mode(), m.ino(), Vec::new())];
    for entry in fs::read_dir(home).unwrap() {
        let entry = entry.unwrap();
        let m = fs::symlink_metadata(entry.path()).unwrap();
        rows.push((
            entry.file_name(),
            m.mode(),
            m.ino(),
            fs::read(entry.path()).unwrap_or_default(),
        ));
    }
    rows.sort();
    rows
}

#[test]
fn antigravity_local_profiles_register_verify_remove() {
    isolated_in(
        PRIVATE_PARENT,
        "antigravity_local_profiles_register_verify_remove",
        &[],
        |root| {
            let registry = config(root).join("provider-accounts.antigravity.json");
            let receipt = config(root).join("antigravity-account-receipts.json");
            let files = || (fs::read(&registry).unwrap(), fs::read(&receipt).unwrap());
            let (a, b) = (
                agy_home(root, "agy-a", true),
                agy_home(root, "agy-b", false),
            );
            let a_spec = ["--account", "antigravity:a", "--home", a.to_str().unwrap()];
            let out = ok(root, &a_spec);
            assert!(
                out.contains("ADC profile present") && out.contains("unverified"),
                "{out}"
            );
            let out = ok(
                root,
                &["--account", "antigravity:b", "--home", b.to_str().unwrap()],
            );
            assert!(out.contains("ADC profile pending"), "{out}");
            assert_eq!(
                rj(&registry),
                json!({"schema":1,"provider":"antigravity",
                    "accounts":[{"id":"a","home":a},{"id":"b","home":b}]})
            );
            let mut want = identity(&a);
            want["st_mode"] = json!(fs::metadata(&a).unwrap().mode());
            assert_eq!(
                rj(&receipt)[0],
                json!({"id":"a","home":a,"identity":want,"verification":"unverified"})
            );
            for file in [&registry, &receipt] {
                assert_eq!(fs::metadata(file).unwrap().mode() & 0o777, 0o600);
            }
            // Routing and peer launch refuse unproved keys in the shared registry.
            assert!(!config(root).join("provider-accounts.json").exists());

            let out = connect(root, &["--account", "antigravity:a", "--verify"]);
            assert_eq!(out.0, 1, "{out:?}");
            assert!(out.1.contains("ok: antigravity account a local profile structure verified; login and served identity unverified"), "{}", out.1);
            assert!(
                out.1
                    .contains("warn: antigravity account b ADC profile missing"),
                "{}",
                out.1
            );
            agy_home(root, "agy-b", true);
            let out = ok(root, &["--account", "antigravity:b", "--verify"]);
            assert!(!out.contains("warn") && !out.contains("logged in"), "{out}");
            let before = files();
            ok(root, &a_spec);
            assert_eq!(files(), before, "idempotent registration writes nothing");

            // Every refusal leaves both files byte-identical.
            let c = agy_home(root, "agy-c", true);
            let link = root.join("alias-a");
            symlink(&a, &link).unwrap();
            let dotted = root.join("agy-b/../agy-c");
            let open = root.join("open");
            mk(&open).unwrap();
            private(&open, 0o777);
            let under_open = agy_home(&open, "agy-d", false);
            for (home, needle) in [
                (link.to_str().unwrap(), "existing physical directory"),
                (dotted.to_str().unwrap(), "existing physical directory"),
                (under_open.to_str().unwrap(), "existing physical directory"),
                ("agy-c", "absolute physical path"),
                (
                    b.to_str().unwrap(),
                    "duplicate provider account id or canonical home",
                ),
            ] {
                refused(
                    root,
                    &["--account", "antigravity:c", "--home", home],
                    needle,
                );
                assert_eq!(files(), before, "{home}");
            }
            refused(
                root,
                &["--account", "antigravity:a", "--home", c.to_str().unwrap()],
                "already registered with another home",
            );
            fs::hard_link(
                a.join("application_default_credentials.json"),
                c.join("linked.json"),
            )
            .unwrap();
            let profile = c.join("application_default_credentials.json");
            fs::remove_file(&profile).unwrap();
            fs::hard_link(c.join("linked.json"), &profile).unwrap();
            let c_spec = ["--account", "antigravity:c", "--home", c.to_str().unwrap()];
            refused(root, &c_spec, "single-link");
            let out = connect(root, &["--account", "antigravity:a", "--verify"]);
            assert!(out.0 == 1 && out.1.contains("warn: antigravity account a antigravity ADC profile must be a private (0600), single-link"), "{out:?}");
            fs::remove_file(&profile).unwrap();
            fs::remove_file(c.join("linked.json")).unwrap();
            assert_eq!(files(), before);
            for (path, mode, needle) in [
                (&c, 0o750, "private (chmod 700)"),
                (&c, 0o770, "existing physical directory"),
                (
                    &a.join("application_default_credentials.json"),
                    0o640,
                    "private (0600)",
                ),
            ] {
                private(path, mode);
                let spec = if path.starts_with(&a) {
                    &a_spec
                } else {
                    &c_spec
                };
                refused(root, spec, needle);
                assert_eq!(files(), before, "{path:?} {mode:o}");
                private(path, if mode == 0o640 { 0o600 } else { 0o700 });
            }
            // A receipt that no longer matches, or is missing, refuses re-registration.
            let mut doc = rj(&receipt);
            doc[0]["identity"]["st_ino"] =
                json!(doc[0]["identity"]["st_ino"].as_u64().unwrap() + 1);
            fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
            let conflict = fs::read(&receipt).unwrap();
            refused(root, &a_spec, "receipt does not match the home identity");
            assert_eq!(fs::read(&receipt).unwrap(), conflict);
            assert_eq!(fs::read(&registry).unwrap(), before.0);
            let out = connect(root, &["--account", "antigravity:a", "--verify"]);
            assert!(
                out.0 == 1 && out.1.contains("no longer matches its receipt"),
                "{out:?}"
            );
            doc.as_array_mut().unwrap().remove(0);
            fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
            let missing = fs::read(&receipt).unwrap();
            refused(root, &a_spec, "missing antigravity account receipt");
            assert_eq!(fs::read(&receipt).unwrap(), missing);
            assert_eq!(fs::read(&registry).unwrap(), before.0);
            fs::write(&receipt, &conflict).unwrap();
            private(&receipt, 0o644);
            refused(root, &a_spec, "must be private, single-link");
            fs::write(&receipt, &before.1).unwrap();
            private(&receipt, 0o600);

            // Codex and Antigravity never share a home, in either direction.
            let (codex_main, _) = seed(root, "codex");
            ok(root, &["--account", "codex:work"]);
            let codex_home = codex_main.join("accounts/work");
            refused(
                root,
                &[
                    "--account",
                    "antigravity:c",
                    "--home",
                    codex_home.to_str().unwrap(),
                ],
                "canonical home is already registered by another provider",
            );
            refused(
                root,
                &["--account", "codex:other", "--home", a.to_str().unwrap()],
                "canonical home is already registered by another provider",
            );
            assert_eq!(files(), before);
            assert_eq!(registered(root, "codex"), ["default", "work"]);
            assert!(
                rj(&config(root).join("provider-accounts.json"))["providers"]["antigravity"]
                    .is_null()
            );

            // Corrupt or inconsistent metadata, ours or foreign, refuses
            // end to end and leaves every config file byte-identical.
            let (claude_main, _) = seed(root, "claude");
            ok(root, &["--account", "claude:work"]);
            assert!(claude_main.join("accounts/work").is_dir());
            let cfg = config(root);
            let intact = |args: &[&str], needle: &str| {
                let before = snapshot(&cfg);
                refused(root, args, needle);
                assert_eq!(snapshot(&cfg), before, "{args:?}");
            };
            let codex_other = root.join("codex-other");
            let codex_spec = [
                "--account",
                "codex:other",
                "--home",
                codex_other.to_str().unwrap(),
            ];
            let edit = |file: &str, bytes: &[u8], check: &dyn Fn()| {
                let path = cfg.join(file);
                let good = fs::read(&path).unwrap();
                fs::write(&path, bytes).unwrap();
                assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
                check();
                fs::write(&path, good).unwrap();
            };
            let agy_specs: [&[&str]; 3] = [
                &c_spec,
                &["--account", "antigravity:a", "--verify"],
                &["--account", "antigravity:a", "--remove"],
            ];
            for file in [
                "provider-accounts.antigravity.json",
                "antigravity-account-receipts.json",
            ] {
                edit(file, b"", &|| {
                    let needle = format!("antigravity account metadata is empty (corrupt): {file}");
                    for spec in agy_specs {
                        intact(spec, &needle);
                    }
                    intact(&codex_spec, &needle);
                });
            }
            for file in ["provider-accounts.json", "codex-account-links.json"] {
                edit(file, b"", &|| {
                    intact(
                        &c_spec,
                        &format!("provider account metadata is empty (corrupt): {file}"),
                    )
                });
            }
            let links = rj(&cfg.join("codex-account-links.json"));
            for (key, value) in [("extra", json!(1)), ("home", json!("relative"))] {
                let mut bad = links.clone();
                bad[0][key] = value;
                edit(
                    "codex-account-links.json",
                    &serde_json::to_vec(&bad).unwrap(),
                    &|| {
                        intact(
                            &c_spec,
                            "invalid provider account metadata (codex-account-links.json)",
                        )
                    },
                );
            }
            let mut bad = rj(&cfg.join("provider-account-effects.json"));
            bad["extra"] = json!(1);
            edit(
                "provider-account-effects.json",
                &serde_json::to_vec(&bad).unwrap(),
                &|| {
                    intact(
                        &c_spec,
                        "invalid provider account metadata (provider-account-effects.json)",
                    )
                },
            );
            edit("codex-account-links.json", b"[]", &|| {
                intact(
                    &c_spec,
                    "missing codex account receipt for work (codex-account-links.json)",
                )
            });
            let mut bad = rj(&receipt);
            bad.as_array_mut().unwrap().retain(|r| r["id"] != "b");
            edit(
                "antigravity-account-receipts.json",
                &serde_json::to_vec(&bad).unwrap(),
                &|| {
                    intact(
                        &codex_spec,
                        "missing antigravity account receipt for b (antigravity-account-receipts.json)",
                    )
                },
            );
            assert_eq!(files(), before);
            assert!(!codex_other.exists());

            // Removal is metadata-only and leaves the profile home untouched.
            let kept = snapshot(&a);
            let out = ok(root, &["--account", "antigravity:a", "--remove"]);
            assert!(out.contains("home and ADC profile kept"), "{out}");
            assert_eq!(snapshot(&a), kept);
            assert_eq!(rj(&registry)["accounts"], json!([{"id":"b","home":b}]));
            assert_eq!(rj(&receipt).as_array().unwrap().len(), 1);
            let after = files();
            let out = ok(root, &["--account", "antigravity:a", "--remove"]);
            assert!(out.contains("nothing changed"), "{out}");
            assert_eq!(files(), after);
            ok(root, &["--account", "antigravity:b", "--remove"]);
            assert_eq!(rj(&registry)["accounts"], json!([]));
            assert_eq!(rj(&receipt), json!([]));
            refused(
                root,
                &["--account", "antigravity:b", "--verify"],
                "not registered",
            );
            // A fresh registration of the removed profile is allowed again.
            ok(root, &a_spec);
        },
    );
}

#[test]
fn antigravity_path_alias_sibling_receipt_and_provider_compat() {
    isolated_in(
        PRIVATE_PARENT,
        "antigravity_path_alias_sibling_receipt_and_provider_compat",
        &[],
        |root| {
            let a = agy_home(root, "agy-a", true);
            let b = agy_home(root, "agy-b", true);
            let c = agy_home(root, "agy-c", true);
            let home_alias = root.join("home-alias");
            symlink(root.join("home"), &home_alias).unwrap();
            let unsafe_cfg = root.join("unsafe-cfg");
            mk(&unsafe_cfg).unwrap();
            private(&unsafe_cfg, 0o777);
            let unsafe_alias = root.join("unsafe-xdg-alias");
            symlink(&unsafe_cfg, &unsafe_alias).unwrap();

            // 1. Aliased XDG parent path resolves to the same physical config root
            // for lock, registry, receipt and private-tree checks.
            let alias_out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "antigravity_path_alias_child", "--nocapture"])
                .env(ROOT, root)
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", home_alias.join(".config"))
                .output()
                .unwrap();
            assert!(
                alias_out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&alias_out.stdout),
                String::from_utf8_lossy(&alias_out.stderr)
            );
            let registry = config(root).join("provider-accounts.antigravity.json");
            let receipt = config(root).join("antigravity-account-receipts.json");
            assert_eq!(rj(&registry)["accounts"], json!([{"id":"a","home":a}]));
            assert_eq!(rj(&receipt).as_array().unwrap().len(), 1);
            assert_eq!(registered(root, "codex"), ["default", "work"]);

            // Unsafe symlinked XDG target is refused cleanly.
            let unsafe_out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "antigravity_unsafe_xdg_alias_child",
                    "--nocapture",
                ])
                .env(ROOT, root)
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", &unsafe_alias)
                .output()
                .unwrap();
            assert!(
                unsafe_out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&unsafe_out.stdout),
                String::from_utf8_lossy(&unsafe_out.stderr)
            );
            assert!(!unsafe_cfg.join("oh-my-setting").exists());

            // 2. Missing or mismatched sibling AGY receipt blocks unrelated
            // registration while explicit recovery of the broken alias succeeds.
            let a_kept = snapshot(&a);
            let b_kept = snapshot(&b);
            let b_spec = ["--account", "antigravity:b", "--home", b.to_str().unwrap()];
            let a_spec = ["--account", "antigravity:a", "--home", a.to_str().unwrap()];
            let good_receipt = fs::read(&receipt).unwrap();
            fs::write(&receipt, b"[]\n").unwrap();
            let before_missing = (fs::read(&registry).unwrap(), fs::read(&receipt).unwrap());
            refused(
                root,
                &b_spec,
                "missing or mismatched antigravity account receipt for a",
            );
            assert_eq!(
                (fs::read(&registry).unwrap(), fs::read(&receipt).unwrap()),
                before_missing
            );
            assert_eq!(snapshot(&b), b_kept);

            let mut mismatched: Value = serde_json::from_slice(&good_receipt).unwrap();
            mismatched[0]["home"] = json!(c);
            fs::write(&receipt, serde_json::to_vec(&mismatched).unwrap()).unwrap();
            let before_mismatch = (fs::read(&registry).unwrap(), fs::read(&receipt).unwrap());
            refused(
                root,
                &b_spec,
                "missing or mismatched antigravity account receipt for a",
            );
            assert_eq!(
                (fs::read(&registry).unwrap(), fs::read(&receipt).unwrap()),
                before_mismatch
            );

            // Explicit recovery: remove broken alias `a`, re-register `a`, then `b`.
            ok(root, &["--account", "antigravity:a", "--remove"]);
            assert_eq!(snapshot(&a), a_kept);
            ok(root, &a_spec);
            ok(root, &b_spec);
            assert_eq!(
                rj(&registry)["accounts"],
                json!([{"id":"a","home":a},{"id":"b","home":b}])
            );

            // 3. Deterministic recovery from on-disk partial publication states
            // (orphaned receipt after write 1, or remaining receipt during --remove).
            let c_kept = snapshot(&c);
            let c_spec = ["--account", "antigravity:c", "--home", c.to_str().unwrap()];
            ok(root, &c_spec);
            let mut reg_without_c = rj(&registry);
            reg_without_c["accounts"]
                .as_array_mut()
                .unwrap()
                .retain(|r| r["id"] != "c");
            fs::write(&registry, serde_json::to_vec(&reg_without_c).unwrap()).unwrap();
            // Re-running registration reconciles `c` into the registry.
            ok(root, &c_spec);
            assert_eq!(snapshot(&c), c_kept);
            assert_eq!(rj(&registry)["accounts"].as_array().unwrap().len(), 3);
            // Partial removal state (removed from registry, still in receipt) recovers via --remove.
            fs::write(&registry, serde_json::to_vec(&reg_without_c).unwrap()).unwrap();
            ok(root, &["--account", "antigravity:c", "--remove"]);
            assert_eq!(snapshot(&c), c_kept);
            assert!(
                !rj(&receipt)
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["id"] == "c")
            );

            // 4. Existing providers (codex, claude, pi) stay completely intact.
            for provider in ["codex", "claude", "pi"] {
                seed(root, provider);
                let spec = format!("{provider}:compat");
                ok(root, &["--account", &spec]);
                assert!(registered(root, provider).contains(&"compat".to_string()));
                ok(root, &["--account", &spec, "--remove"]);
                assert!(!registered(root, provider).contains(&"compat".to_string()));
            }
            assert!(
                rj(&config(root).join("provider-accounts.json"))["providers"]["antigravity"]
                    .is_null()
            );
        },
    );
}

#[test]
fn antigravity_path_alias_child() {
    if std::env::var_os(ROOT).is_none() {
        return;
    }
    let root = fixture_root(
        PRIVATE_PARENT,
        "antigravity_path_alias_sibling_receipt_and_provider_compat",
        None,
        None,
        Some("home-alias/.config"),
    );
    let a = root.join("agy-a");
    ok(
        &root,
        &["--account", "antigravity:a", "--home", a.to_str().unwrap()],
    );
    seed(&root, "codex");
    ok(&root, &["--account", "codex:work"]);
    refused(
        &root,
        &["--account", "codex:clash", "--home", a.to_str().unwrap()],
        "already registered by another provider",
    );
}

#[test]
fn antigravity_unsafe_xdg_alias_child() {
    if std::env::var_os(ROOT).is_none() {
        return;
    }
    let root = fixture_root(
        PRIVATE_PARENT,
        "antigravity_path_alias_sibling_receipt_and_provider_compat",
        None,
        None,
        Some("unsafe-xdg-alias"),
    );
    let b = root.join("agy-b");
    refused(
        &root,
        &["--account", "antigravity:b", "--home", b.to_str().unwrap()],
        "unsafe antigravity account registry ancestry",
    );
}

#[test]
fn antigravity_reserves_legacy_claude_default_home() {
    isolated_in(
        PRIVATE_PARENT,
        "antigravity_reserves_legacy_claude_default_home",
        &[],
        |root| {
            let (claude_main, _) = seed(root, "claude");
            ok(root, &["--account", "claude:work"]);
            // Make $HOME/.claude satisfy agy_home's 0700 check so only foreign_homes can refuse it.
            private(&claude_main, 0o700);
            // Remove the shared "claude" section so only legacy provider-accounts.claude.json remains.
            let shared_path = config(root).join("provider-accounts.json");
            let mut shared = rj(&shared_path);
            shared["providers"]
                .as_object_mut()
                .unwrap()
                .remove("claude");
            fs::write(&shared_path, serde_json::to_vec(&shared).unwrap()).unwrap();
            assert!(
                !rj(&config(root).join("provider-accounts.claude.json"))["accounts"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let claude_override = root.join("claude-override");
            mk(&claude_override).unwrap();
            let distinct = agy_home(root, "agy-distinct", true);

            let out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "antigravity_legacy_claude_default_home_child",
                    "--nocapture",
                ])
                .env(ROOT, root)
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("home/.config"))
                .env("CLAUDE_CONFIG_DIR", &claude_override)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let registry = config(root).join("provider-accounts.antigravity.json");
            let receipt = config(root).join("antigravity-account-receipts.json");
            assert_eq!(rj(&registry)["accounts"], json!([]));
            assert_eq!(rj(&receipt), json!([]));
            assert!(distinct.is_dir());
        },
    );
}

#[test]
fn antigravity_legacy_claude_default_home_child() {
    if std::env::var_os(ROOT).is_none() {
        return;
    }
    let root = fixture_root(
        PRIVATE_PARENT,
        "antigravity_reserves_legacy_claude_default_home",
        None,
        Some("claude-override"),
        None,
    );
    let claude_main = root.join("home/.claude");
    let distinct = root.join("agy-distinct");
    let cfg = config(&root);
    let before = snapshot(&cfg);
    // With CLAUDE_CONFIG_DIR overridden, canonical $HOME/.claude is still reserved
    // by the implicit legacy Claude default home.
    refused(
        &root,
        &[
            "--account",
            "antigravity:clash",
            "--home",
            claude_main.to_str().unwrap(),
        ],
        "canonical home is already registered by another provider",
    );
    assert_eq!(snapshot(&cfg), before);

    // Ordinary registration targeting a distinct synthetic home succeeds.
    let distinct_spec = [
        "--account",
        "antigravity:ok",
        "--home",
        distinct.to_str().unwrap(),
    ];
    ok(&root, &distinct_spec);
    let registry = cfg.join("provider-accounts.antigravity.json");
    let receipt = cfg.join("antigravity-account-receipts.json");
    assert_eq!(
        rj(&registry)["accounts"],
        json!([{"id":"ok","home":distinct}])
    );
    assert_eq!(rj(&receipt).as_array().unwrap().len(), 1);

    // Explicit --remove / recovery remains unaffected.
    let kept = snapshot(&distinct);
    ok(&root, &["--account", "antigravity:ok", "--remove"]);
    assert_eq!(snapshot(&distinct), kept);
    assert_eq!(rj(&registry)["accounts"], json!([]));
    assert_eq!(rj(&receipt), json!([]));
}
