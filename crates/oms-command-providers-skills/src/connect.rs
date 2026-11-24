use crate::common::*;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn run(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        raw,
        &["--data-source-id", "--account", "--home", "--login"],
        &[
            "--auto",
            "--required",
            "--no-github",
            "--no-notion",
            "--first",
            "--last",
            "--repair",
            "--remove",
            "--verify",
            "--login-menu",
        ],
    )?;
    args.count(0)?;
    host_mutation()?;
    if args.get("--login").is_some() || args.has("--login-menu") {
        if args.get("--login").is_some() && args.has("--login-menu") {
            return Err(error("choose --login PROVIDER[:ID] or --login-menu"));
        }
        if args.get("--account").is_some()
            || args.get("--home").is_some()
            || args.get("--data-source-id").is_some()
            || [
                "--auto",
                "--required",
                "--no-github",
                "--no-notion",
                "--first",
                "--last",
                "--repair",
                "--remove",
                "--verify",
            ]
            .iter()
            .any(|s| args.has(s))
        {
            return Err(error(
                "login options cannot be combined with account or GitHub/Notion options",
            ));
        }
        if let Some(target) = args.get("--login") {
            return login_direct(target, ctx);
        }
        return login_menu(ctx);
    }
    if args.get("--account").is_some() {
        if args.get("--data-source-id").is_some()
            || ["--auto", "--required", "--no-github", "--no-notion"]
                .iter()
                .any(|s| args.has(s))
        {
            return Err(error(
                "account options cannot be combined with GitHub/Notion options",
            ));
        }
        return account(&args, ctx);
    }
    if args.get("--home").is_some()
        || ["--first", "--last", "--repair", "--remove", "--verify"]
            .iter()
            .any(|s| args.has(s))
    {
        return Err(error("account options require --account PROVIDER:ID"));
    }
    let interactive = match std::env::var("OMS_CONNECT_INTERACTIVE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        Ok("auto") | Err(_) => std::io::stdin().is_terminal() || std::io::stdout().is_terminal(),
        _ => return Err(error("OMS_CONNECT_INTERACTIVE must be 0, 1, or auto")),
    };
    let mut pending = false;
    let mut out = String::new();
    let mut err = String::new();
    let mut problem = |message: &str| {
        pending = true;
        err.push_str(&format!(
            "{}: {message}\n",
            if args.has("--required") {
                "error"
            } else {
                "note"
            }
        ));
    };
    if !args.has("--no-github") {
        let bin = std::env::var("OMS_GH_BIN").unwrap_or_else(|_| "gh".into());
        let mut good = installed(&bin) && ctx.command(&bin, &["auth", "status"], 20)?.code == 0;
        if !good && interactive && installed(&bin) {
            let status = Command::new(&bin)
                .args(["auth", "login", "--web"])
                .current_dir(ctx.cwd)
                .status()?;
            good = status.success() && ctx.command(&bin, &["auth", "status"], 20)?.code == 0;
        }
        if good {
            out.push_str("ok: GitHub CLI authenticated\n");
        } else {
            problem("GitHub login pending; run: gh auth login --web");
        }
    }
    if !args.has("--no-notion") {
        let bin = std::env::var("OMS_NOTION_CLI").unwrap_or_else(|_| "ntn".into());
        let mut keyring = std::env::var("OMS_NOTION_KEYRING").unwrap_or_default();
        if installed(&bin) {
            let probe = ctx.command(&bin, &["whoami"], 20)?;
            let mut status = probe.out;
            status.extend_from_slice(&probe.err);
            if String::from_utf8_lossy(&status)
                .to_ascii_lowercase()
                .contains("keychain")
            {
                keyring = "file".into();
            }
            status.fill(0);
        }
        let verified = notion_api(&bin, &keyring, &["api", concat!("v1/user", "s/me")], ctx)
            .is_ok_and(|r| r["object"] == "user");
        let mut verified = verified;
        if !verified && interactive && installed(&bin) {
            let mut command = Command::new(&bin);
            command.arg("login");
            if keyring == "file" {
                command.env("NOTION_KEYRING", "0");
            }
            verified = command.status()?.success()
                && notion_api(&bin, &keyring, &["api", concat!("v1/user", "s/me")], ctx)
                    .is_ok_and(|r| r["object"] == "user");
        }
        let supplied = args.get("--data-source-id");
        if !verified {
            if let Some(id) = supplied {
                configure(id, &bin, &keyring)?;
            }
            problem("Notion login pending; run: ntn login, then: oms journal configure --discover");
        } else {
            let target = if let Some(id) = supplied {
                if compatible(&bin, &keyring, id, ctx)? {
                    Some(id.to_owned())
                } else {
                    None
                }
            } else {
                discover(&bin, &keyring, ctx)?
            };
            if let Some(id) = target {
                configure(&id, &bin, &keyring)?;
                out.push_str("ok: Notion Work Journal connected\n");
            } else {
                problem(
                    "no unique compatible Work Journal data source was found; rerun with --data-source-id ID",
                );
            }
        }
    }
    out.push_str(if pending {
        "service connection: pending\n"
    } else {
        "service connection: ok\n"
    });
    Ok(Output {
        code: i32::from(pending && args.has("--required")),
        out: out.into_bytes(),
        err: err.into_bytes(),
    })
}
fn notion_api(bin: &str, keyring: &str, args: &[&str], ctx: &Context<'_>) -> Result<Value> {
    let mut command = Command::new(bin);
    command.args(args).current_dir(ctx.cwd);
    if keyring == "file" {
        command.env("NOTION_KEYRING", "0");
    }
    let mut output = capture(command, ctx.interrupted, 30)?;
    if output.code != 0 {
        output.out.fill(0);
        output.err.fill(0);
        return Err(error("Notion CLI request failed"));
    }
    let row = json(&output.out);
    output.out.fill(0);
    output.err.fill(0);
    row
}
fn valid_id(id: &str) -> bool {
    let compact = id.replace('-', "");
    compact.len() == 32
        && compact.bytes().all(|b| b.is_ascii_hexdigit())
        && (id.len() == 32 || id.len() == 36)
}
fn compatible(bin: &str, keyring: &str, id: &str, ctx: &Context<'_>) -> Result<bool> {
    if !valid_id(id) {
        return Err(error("invalid Notion data source id"));
    }
    let endpoint = format!("v1/data_sources/{id}");
    let row = notion_api(bin, keyring, &["api", &endpoint], ctx)?;
    Ok([
        ("Name", "title"),
        ("Work Journal Key", "rich_text"),
        ("Content Hash", "rich_text"),
        ("Project", "rich_text"),
        ("Kind", "select"),
        ("Period", "date"),
        ("Has Blocker", "checkbox"),
    ]
    .iter()
    .all(|(name, kind)| row["properties"][*name]["type"] == *kind))
}
fn discover(bin: &str, keyring: &str, ctx: &Context<'_>) -> Result<Option<String>> {
    let mut cursor = String::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = vec![];
    for _ in 0..100 {
        let scratch = crate::lifecycle::Scratch::new("notion-discover")?;
        let path = scratch.0.join("query");
        let mut body =
            json!({"filter":{"property":"object","value":"data_source"},"page_size":100});
        if !cursor.is_empty() {
            body["start_cursor"] = json!(cursor);
        }
        std::fs::write(&path, body.to_string())?;
        let mut command = Command::new(bin);
        command
            .args(["api", "v1/search", "-X", "POST"])
            .stdin(std::fs::File::open(path)?);
        if keyring == "file" {
            command.env("NOTION_KEYRING", "0");
        }
        let output = capture(command, ctx.interrupted, 30)?;
        if output.code != 0 {
            return Err(error("Notion discovery failed"));
        }
        let row = json(&output.out)?;
        for entry in row["results"]
            .as_array()
            .ok_or_else(|| error("invalid Notion search response"))?
        {
            if entry["object"] == "data_source"
                && let Some(id) = entry["id"].as_str()
                && compatible(bin, keyring, id, ctx)?
            {
                candidates.push(id.to_owned());
            }
        }
        if row["has_more"] != true {
            candidates.sort();
            candidates.dedup();
            return Ok(if candidates.len() == 1 {
                candidates.pop()
            } else {
                None
            });
        }
        cursor = row["next_cursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("missing Notion search cursor"))?
            .to_owned();
        if !seen.insert(cursor.clone()) {
            return Err(error("Notion search cursor repeated"));
        }
    }
    Err(error("Notion discovery exceeded page budget"))
}
fn configure(id: &str, bin: &str, keyring: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(error("invalid Notion data source id"));
    }
    let path = env_path(
        "OMS_WORK_JOURNAL_CONFIG",
        env_path("XDG_CONFIG_HOME", home()?.join(".config"))
            .join("oh-my-setting/work-journal.json"),
    );
    let _guard = lock(&path)?;
    let old = optional(&path)?;
    let prior = if old.is_empty() {
        json!({})
    } else {
        json(&old)?
    };
    let mut row = json!({"schema_version":1,"managed_by":"oh-my-setting","notion":{"data_source_id":id,"auth_mode":"ntn","cli_command":bin,"sync_mode":"finalized","properties":{"title":"Name","key":"Work Journal Key","hash":"Content Hash","project":"Project","kind":"Kind","period":"Period","blocker":"Has Blocker","sessions":"Sessions","commits":"Commits","verified":"Verified"}}});
    if keyring == "file" {
        row["notion"]["keyring"] = json!("file");
    }
    if let Some(language) = prior["language"]
        .as_str()
        .filter(|s| ["ko", "en"].contains(s))
    {
        row["language"] = json!(language);
    }
    if let Some(excluded) = prior["notion"]["excluded_repos"].as_array()
        && excluded.iter().all(Value::is_string)
    {
        row["notion"]["excluded_repos"] = json!(excluded);
    }
    write_locked(&path, &json_bytes(&row)?, false, Some(&old))
}
fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}
fn validate_account_row(row: &Value, allow_default: bool) -> Result<(&str, &str)> {
    let Some(obj) = row.as_object() else {
        return Err(error("invalid provider accounts"));
    };
    if !obj.keys().all(|k| ["id", "home"].contains(&k.as_str())) {
        return Err(error("invalid provider accounts"));
    }
    let Some(id) = row["id"]
        .as_str()
        .filter(|s| valid_account_id(s) && (allow_default || *s != "default"))
    else {
        return Err(error("invalid provider accounts"));
    };
    let Some(home) = row["home"].as_str().filter(|h| Path::new(h).is_absolute()) else {
        return Err(error("invalid provider accounts"));
    };
    Ok((id, home))
}
fn validate_shared_registry(value: &Value) -> Result<std::collections::BTreeSet<PathBuf>> {
    if value["schema"] != 1
        || !value.as_object().is_some_and(|m| {
            m.keys()
                .all(|k| ["schema", "providers"].contains(&k.as_str()))
        })
        || !value["providers"].is_object()
    {
        return Err(error("unsupported provider accounts schema"));
    }
    let mut all_homes = std::collections::BTreeSet::new();
    for (owner, settings) in value["providers"].as_object().unwrap() {
        if !["codex", "claude", "pi"].contains(&owner.as_str()) {
            return Err(error("provider account isolation is unproved"));
        }
        if !settings.as_object().is_some_and(|m| {
            m.keys()
                .all(|k| ["policy", "accounts"].contains(&k.as_str()))
        }) || settings.get("policy").is_some_and(|p| {
            p.as_str()
                .is_none_or(|s| !["ordered", "spread"].contains(&s))
        }) {
            return Err(error("invalid provider accounts"));
        }
        let Some(entries) = settings["accounts"].as_array().filter(|a| !a.is_empty()) else {
            return Err(error("invalid provider accounts"));
        };
        let mut ids = std::collections::BTreeSet::new();
        for row in entries {
            let (rid, rhome) = validate_account_row(row, true)?;
            let canon = physical(Path::new(rhome))?;
            if !ids.insert(rid.to_owned()) || !all_homes.insert(canon) {
                return Err(error("duplicate provider account id or canonical home"));
            }
        }
    }
    Ok(all_homes)
}
fn validate_claude_registry(
    doc: &Value,
    default_claude: &Path,
    shared: &Value,
    shared_homes: &std::collections::BTreeSet<PathBuf>,
) -> Result<()> {
    if doc["schema"] != 1
        || doc["provider"] != "claude"
        || !doc.as_object().is_some_and(|m| {
            m.keys()
                .all(|k| ["schema", "provider", "policy", "accounts"].contains(&k.as_str()))
        })
        || doc.get("policy").is_some_and(|p| {
            p.as_str()
                .is_none_or(|s| !["ordered", "spread"].contains(&s))
        })
    {
        return Err(error("invalid Claude registry"));
    }
    let Some(accounts) = doc["accounts"].as_array() else {
        return Err(error("invalid Claude registry"));
    };
    for row in accounts {
        validate_account_row(row, false).map_err(|_| error("invalid Claude registry"))?;
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut homes = std::collections::BTreeSet::new();
    homes.insert(default_claude.to_owned());
    for row in accounts {
        let rid = row["id"].as_str().unwrap_or_default();
        let rhome = physical(Path::new(row["home"].as_str().unwrap_or_default()))?;
        if !ids.insert(rid.to_owned()) || !homes.insert(rhome.clone()) {
            return Err(error("duplicate provider account id or canonical home"));
        }
        if shared["providers"].get("claude").is_none() && shared_homes.contains(&rhome) {
            return Err(error("duplicate provider account id or canonical home"));
        }
    }
    if !accounts.is_empty()
        && shared["providers"].get("claude").is_none()
        && shared_homes.contains(default_claude)
    {
        return Err(error("duplicate provider account id or canonical home"));
    }
    Ok(())
}
fn account_flags(args: &Args, id: &str) -> Result<()> {
    if !valid_account_id(id) || (id == "default" && !args.has("--verify")) {
        return Err(error("invalid provider account id; default is reserved"));
    }
    if args.has("--first") && args.has("--last") {
        return Err(error("choose --first or --last"));
    }
    Ok(())
}
/// Single physical config root for account locks, registries and receipts.
/// Follows the XDG Base Directory Specification (relative or empty
/// `XDG_CONFIG_HOME` falls back to `$HOME/.config`) and canonicalizes once so
/// parent path aliases (such as `/var` -> `/private/var`) resolve consistently.
fn account_config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(path) => path,
        None => home()?.join(".config"),
    };
    physical(&base.join("oh-my-setting"))
}
fn account(args: &Args, ctx: &Context<'_>) -> Result<Output> {
    let (provider, id) = args
        .required("--account")?
        .split_once(':')
        .ok_or_else(|| error("--account requires PROVIDER:ID"))?;
    let (label, env, default, names, receipt_name): (&str, &str, &str, &[&str], &str) =
        match provider {
            "codex" => (
                "Codex",
                "CODEX_HOME",
                ".codex",
                SHARED,
                "codex-account-links.json",
            ),
            "claude" => (
                "Claude",
                "CLAUDE_CONFIG_DIR",
                ".claude",
                CLAUDE_SHARED,
                "provider-account-effects.json",
            ),
            "pi" => (
                "Pi",
                "PI_CODING_AGENT_DIR",
                ".pi/agent",
                PI_SHARED,
                "pi-account-links.json",
            ),
            "antigravity" => return antigravity(args, id),
            _ => {
                return Err(error(
                    "provider account isolation is unproved and unavailable",
                ));
            }
        };
    let claude = provider == "claude";
    account_flags(args, id)?;
    let config = account_config_dir()?;
    let registry = config.join("provider-accounts.json");
    let claude_registry = config.join("provider-accounts.claude.json");
    let receipt = config.join(receipt_name);
    let _guard = lock(&registry)?;
    let old = optional(&registry)?;
    let mut value = if old.is_empty() {
        json!({"schema":1,"providers":{}})
    } else {
        json(&old)?
    };
    let shared_homes = validate_shared_registry(&value)?;
    let rows_raw = optional(&receipt)?;
    let mut receipt_doc = if rows_raw.is_empty() {
        if claude {
            json!({"schema":1,"accounts":[]})
        } else {
            json!([])
        }
    } else {
        json(&rows_raw)?
    };
    if claude && receipt_doc["schema"] != 1 {
        return Err(error("invalid Claude account effects"));
    }
    let mut rows = if claude {
        receipt_doc["accounts"].take()
    } else {
        receipt_doc.take()
    };
    let rows = rows
        .as_array_mut()
        .ok_or_else(|| error(format!("invalid {label} link ownership receipt")))?;
    let save = |rows: &Vec<Value>| -> Result<()> {
        let doc = if claude {
            json!({"schema":1,"accounts":rows})
        } else {
            json!(rows)
        };
        write_locked(&receipt, &json_bytes(&doc)?, false, None)
    };
    let default_claude = physical(&home()?.join(".claude"))?;
    let claude_raw = optional(&claude_registry)?;
    let mut claude_doc = if claude || (!args.has("--verify") && !args.has("--remove")) {
        if claude_raw.is_empty() {
            json!({"schema":1,"provider":"claude","accounts":[]})
        } else {
            let parsed = json(&claude_raw)?;
            validate_claude_registry(&parsed, &default_claude, &value, &shared_homes)?;
            parsed
        }
    } else {
        json!({"schema":1,"provider":"claude","accounts":[]})
    };
    let main = physical(&env_path(env, home()?.join(default)))?;
    let existing = value["providers"][provider]["accounts"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["id"] == id))
        .cloned()
        .or_else(|| {
            claude
                .then(|| {
                    claude_doc["accounts"]
                        .as_array()
                        .and_then(|v| v.iter().find(|v| v["id"] == id))
                        .cloned()
                })
                .flatten()
        });
    if args.has("--verify") {
        let all = value["providers"][provider]["accounts"]
            .as_array()
            .or_else(|| {
                claude
                    .then(|| claude_doc["accounts"].as_array().filter(|v| !v.is_empty()))
                    .flatten()
            })
            .ok_or_else(|| error(format!("{label} account is not registered")))?;
        if claude {
            for var in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ] {
                if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
                    return Err(error(
                        "credential environment overrides the selected Claude account",
                    ));
                }
            }
            for row in std::iter::once(json!({"id":"main","home":main})).chain(all.iter().cloned())
            {
                let path = PathBuf::from(
                    row["home"]
                        .as_str()
                        .ok_or_else(|| error("invalid account home"))?,
                );
                let good = private_home(&path).is_ok();
                if good && installed("claude") {
                    let _ = login_status("claude", Some(&path), ctx)?;
                }
            }
            return Err(error(
                "Claude account credential origin cannot be attested; manual claude.ai isolation remains unverified",
            ));
        }
        if provider == "pi" {
            for row in std::iter::once(json!({"id":"main","home":main})).chain(all.iter().cloned())
            {
                let path = PathBuf::from(
                    row["home"]
                        .as_str()
                        .ok_or_else(|| error("invalid account home"))?,
                );
                let _ = private_home(&path).is_ok();
            }
            return Err(error(
                "Pi account credential origin cannot be attested without a native auth status probe",
            ));
        }
        let mut text = String::new();
        let mut failed = false;
        for row in std::iter::once(json!({"id":"main","home":main})).chain(all.iter().cloned()) {
            let path = PathBuf::from(
                row["home"]
                    .as_str()
                    .ok_or_else(|| error("invalid account home"))?,
            );
            let good = private_home(&path).is_ok();
            let verified =
                good && login_status(provider, Some(&path), ctx)? == LoginStatus::SignedIn;
            failed |= !verified;
            text.push_str(&format!(
                "{}: {provider} account {} {}\n",
                if verified { "ok" } else { "warn" },
                row["id"].as_str().unwrap_or("unknown"),
                if verified {
                    "logged in"
                } else {
                    "login unverified"
                }
            ));
        }
        let mut out = Output::text(text);
        out.code = i32::from(failed);
        return Ok(out);
    }
    if args.has("--remove") {
        let mut validated = Vec::new();
        let mut kept = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if row["id"] != id {
                continue;
            }
            let path = PathBuf::from(
                row["home"]
                    .as_str()
                    .ok_or_else(|| error("invalid receipt home"))?,
            );
            let links = validate_receipt_links(row, names, claude)?;
            if !path.exists() {
                validated.push((index, Vec::new()));
                continue;
            }
            let directory = open_home(&path, Some(if claude { &row["identity"] } else { row }))?;
            if claude {
                use std::io::Read;
                match rustix::fs::openat(
                    &directory,
                    ".oms-claude-hooks.json",
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                ) {
                    Err(rustix::io::Errno::NOENT) => (),
                    Ok(fd) => {
                        let info = rustix::fs::fstat(&fd)
                            .map_err(|_| error("unsafe account hooks receipt"))?;
                        if rustix::fs::FileType::from_raw_mode(info.st_mode)
                            != rustix::fs::FileType::RegularFile
                        {
                            return Err(error("unsafe account hooks receipt"));
                        }
                        let mut raw = Vec::new();
                        std::fs::File::from(fd)
                            .take(1024 * 1024 + 1)
                            .read_to_end(&mut raw)?;
                        if raw.len() > 1024 * 1024 || json(&raw)?["status"] != "removed" {
                            return Err(error(
                                "account hooks are installed; remove them before removing the account",
                            ));
                        }
                    }
                    Err(_) => return Err(error("unsafe account hooks receipt")),
                }
            }
            let mut ops = Vec::new();
            for (name, recorded) in &links {
                let expected = if claude {
                    recorded["target"].as_str()
                } else {
                    recorded.as_str()
                }
                .ok_or_else(|| error("invalid recorded link target"))?;
                let Some((fd, leaf_name)) = leaf(&directory, name, false)? else {
                    ops.push((name.clone(), None));
                    continue;
                };
                match rustix::fs::readlinkat(&fd, leaf_name, Vec::new()) {
                    Ok(actual)
                        if actual.as_bytes() == expected.as_bytes()
                            && (!claude
                                || rustix::fs::statat(
                                    &fd,
                                    leaf_name,
                                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                                )
                                .is_ok_and(|s| stat_id(&s) == recorded["identity"])) =>
                    {
                        ops.push((name.clone(), Some((fd, leaf_name.to_owned()))));
                    }
                    Err(rustix::io::Errno::NOENT) => ops.push((name.clone(), None)),
                    _ => kept.push(name.clone()),
                }
            }
            validated.push((index, ops));
        }
        if let Some(reg) = &existing {
            let expected_home = physical(Path::new(
                reg["home"]
                    .as_str()
                    .ok_or_else(|| error("invalid account home"))?,
            ))?;
            let has_receipt = validated.iter().any(|(idx, _)| {
                rows[*idx]["id"] == id
                    && rows[*idx]["home"]
                        .as_str()
                        .and_then(|s| physical(Path::new(s)).ok())
                        .as_ref()
                        == Some(&expected_home)
            });
            if !has_receipt {
                return Err(error(
                    "missing account ownership receipt; run --repair before removing the account",
                ));
            }
        }
        if !kept.is_empty() {
            kept.sort();
            kept.dedup();
            return Err(error(format!(
                "changed account links kept ({}); nothing was removed — run --repair to adopt or restore links first",
                kept.join(", ")
            )));
        }
        for (index, ops) in validated {
            for (name, target) in ops {
                if let Some((fd, leaf_name)) = target {
                    match rustix::fs::unlinkat(&fd, &leaf_name, rustix::fs::AtFlags::empty()) {
                        Ok(()) | Err(rustix::io::Errno::NOENT) => (),
                        Err(_) => return Err(error("safe account link removal failed")),
                    }
                }
                rows[index]["links"]
                    .as_object_mut()
                    .ok_or_else(|| error("invalid receipt links"))?
                    .remove(&name);
                save(rows)?;
            }
        }
        if let Some(entries) = value["providers"]
            .get_mut(provider)
            .and_then(|section| section["accounts"].as_array_mut())
        {
            entries.retain(|r| r["id"] != id);
            if entries.is_empty() {
                value["providers"]
                    .as_object_mut()
                    .ok_or_else(|| error("invalid providers"))?
                    .remove(provider);
            }
        }
        if claude && !claude_raw.is_empty() {
            claude_doc["accounts"]
                .as_array_mut()
                .ok_or_else(|| error("invalid Claude registry"))?
                .retain(|r| r["id"] != id);
            write_locked(
                &claude_registry,
                &json_bytes(&claude_doc)?,
                false,
                Some(&claude_raw),
            )?;
        }
        write_locked(&registry, &json_bytes(&value)?, false, Some(&old))?;
        rows.retain(|r| r["id"] != id);
        save(rows)?;
        return Ok(Output::text(format!(
            "{label} account removed: {id}; home, login and sessions kept\n"
        )));
    }
    let path = args
        .get("--home")
        .map(|s| ctx.path(s))
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|r| r["home"].as_str())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| main.join("accounts").join(id));
    let path = physical(&path)?;
    home_placement(&path, Some(&main))?;
    for (owner, settings) in value["providers"]
        .as_object()
        .ok_or_else(|| error("invalid providers"))?
    {
        for row in settings["accounts"]
            .as_array()
            .ok_or_else(|| error("invalid provider accounts"))?
        {
            if !(owner == provider && row["id"] == id)
                && row["home"]
                    .as_str()
                    .is_some_and(|s| physical(Path::new(s)).is_ok_and(|p| p == path))
            {
                return Err(error("duplicate provider account id or canonical home"));
            }
        }
    }
    let claude_accounts = claude_doc["accounts"]
        .as_array()
        .ok_or_else(|| error("invalid Claude registry"))?;
    for row in claude_accounts {
        if !(claude && row["id"] == id)
            && row["home"]
                .as_str()
                .is_some_and(|s| physical(Path::new(s)).is_ok_and(|p| p == path))
        {
            return Err(error(
                "canonical home is already registered by another provider",
            ));
        }
    }
    for (owner_prov, file, nested) in [
        ("codex", "codex-account-links.json", false),
        ("pi", "pi-account-links.json", false),
        ("claude", "provider-account-effects.json", true),
    ] {
        let raw_receipt = optional(&config.join(file))?;
        if raw_receipt.is_empty() {
            continue;
        }
        let parsed = json(&raw_receipt)?;
        let list = if nested {
            parsed["accounts"].as_array()
        } else {
            parsed.as_array()
        }
        .ok_or_else(|| error("invalid provider account receipt"))?;
        for row in list {
            if !(owner_prov == provider && row["id"] == id)
                && let Some(canon) = row["home"]
                    .as_str()
                    .and_then(|s| physical(Path::new(s)).ok())
            {
                if value["providers"].get(provider).is_none()
                    && owner_prov == provider
                    && canon == main
                {
                    return Err(error("duplicate provider account id or canonical home"));
                }
                if canon == path || (value["providers"].get(provider).is_none() && canon == main) {
                    return Err(error(
                        "canonical home is already registered by another provider",
                    ));
                }
            }
        }
    }
    for canon in agy_homes(&config)? {
        if canon == path || (value["providers"].get(provider).is_none() && canon == main) {
            return Err(error(
                "canonical home is already registered by another provider",
            ));
        }
    }
    let mut next_value = value.clone();
    let mut next_claude_doc = claude_doc.clone();
    if next_value["providers"].get(provider).is_none() {
        let mut seeded = vec![json!({"id":"default","home":main})];
        if claude && let Some(prior) = next_claude_doc["accounts"].as_array() {
            seeded.extend(prior.iter().cloned());
        }
        let policy = if claude {
            next_claude_doc["policy"].as_str().unwrap_or("ordered")
        } else {
            "ordered"
        };
        next_value["providers"][provider] = json!({"policy":policy,"accounts":seeded});
    }
    let entries = next_value["providers"][provider]["accounts"]
        .as_array_mut()
        .ok_or_else(|| error("invalid provider accounts"))?;
    entries.retain(|r| r["id"] != id);
    let entry = json!({"id":id,"home":path});
    if args.has("--first") {
        entries.insert(0, entry.clone());
    } else {
        entries.push(entry.clone());
    }
    if claude {
        let list = next_claude_doc["accounts"]
            .as_array_mut()
            .ok_or_else(|| error("invalid Claude registry"))?;
        list.retain(|r| r["id"] != id);
        if args.has("--first") {
            list.insert(0, entry);
        } else {
            list.push(entry);
        }
    }
    let next_shared_homes = validate_shared_registry(&next_value)?;
    if claude {
        validate_claude_registry(
            &next_claude_doc,
            &default_claude,
            &next_value,
            &next_shared_homes,
        )?;
    }
    for row in rows.iter() {
        if row["id"] == id
            || row["home"]
                .as_str()
                .and_then(|s| physical(Path::new(s)).ok())
                .as_ref()
                == Some(&path)
        {
            validate_receipt_links(row, names, claude)?;
        }
    }
    let owner = rows
        .iter()
        .find(|r| r["id"] == id && r["home"] == path.to_string_lossy().as_ref())
        .cloned();
    let parent = path
        .parent()
        .ok_or_else(|| error("invalid account parent"))?;
    private_parents(parent)?;
    if !path.exists() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)?;
    }
    private_home(&path)?;
    let directory = open_home(
        &path,
        owner
            .as_ref()
            .map(|r| if claude { &r["identity"] } else { r }),
    )?;
    let info =
        rustix::fs::fstat(&directory).map_err(|_| error("cannot inspect account directory"))?;
    // Preflight every subdirectory and (--repair) symlink before mutating receipts or links.
    for name in names {
        let target = main.join(name);
        if provider != "codex" && std::fs::symlink_metadata(&target).is_err() {
            continue;
        }
        let Some((fd, leaf_name)) = leaf(&directory, name, false)? else {
            continue;
        };
        if !args.has("--repair") {
            continue;
        }
        let Ok(info) = rustix::fs::statat(&fd, leaf_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        else {
            continue;
        };
        if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::Symlink {
            continue;
        }
        let current = rustix::fs::readlinkat(&fd, leaf_name, Vec::new())
            .map_err(|_| error("cannot inspect account link"))?;
        if current.as_bytes() == target.as_os_str().as_encoded_bytes() {
            continue;
        }
        let recorded = owner.as_ref().map(|row| &row["links"][*name]);
        let recorded_target = recorded.and_then(|rec| {
            if claude {
                rec["target"].as_str()
            } else {
                rec.as_str()
            }
        });
        if recorded_target != std::str::from_utf8(current.as_bytes()).ok()
            || (claude && recorded.is_none_or(|rec| rec["identity"] != stat_id(&info)))
        {
            return Err(error(
                "unowned conflicting account link kept; no links or receipts changed; remove or adopt the conflicting link before --repair",
            ));
        }
    }
    if owner.is_none() {
        rows.push(if claude {
            json!({"id":id,"provider":"claude","home":path,"parent":parent,
                "parent_identity":path_id(parent)?,"identity":stat_id(&info),
                "settings":path.join("settings.json"),
                "hooks_receipt":path.join(".oms-claude-hooks.json"),
                "source_root":physical(ctx.source)?,"status":"installed","links":{}})
        } else {
            json!({"id":id,"home":path,"links":{},"st_dev":info.st_dev,"st_ino":info.st_ino})
        });
        save(rows)?;
    }
    let index = rows
        .iter()
        .position(|r| r["id"] == id && r["home"] == path.to_string_lossy().as_ref())
        .ok_or_else(|| error("missing account ownership"))?;
    for name in names {
        let target = main.join(name);
        if provider != "codex" && std::fs::symlink_metadata(&target).is_err() {
            continue;
        }
        let Some((fd, leaf_name)) = leaf(&directory, name, true)? else {
            continue;
        };
        let current = rustix::fs::statat(&fd, leaf_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW);
        if let Ok(info) = current {
            if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::Symlink {
                continue;
            }
            let current = rustix::fs::readlinkat(&fd, leaf_name, Vec::new())
                .map_err(|_| error("cannot inspect account link"))?;
            if current.as_bytes() == target.as_os_str().as_encoded_bytes() {
                if args.has("--repair") {
                    rows[index]["links"]
                        .as_object_mut()
                        .ok_or_else(|| error("invalid receipt links"))?
                        .insert(
                            (*name).to_owned(),
                            if claude {
                                json!({"target":target,"identity":stat_id(&info)})
                            } else {
                                json!(target)
                            },
                        );
                    save(rows)?;
                }
                continue;
            }
            if !args.has("--repair") {
                continue;
            }
            let recorded = &rows[index]["links"][*name];
            let recorded_target = if claude {
                recorded["target"].as_str()
            } else {
                recorded.as_str()
            };
            if recorded_target != std::str::from_utf8(current.as_bytes()).ok()
                || (claude && recorded["identity"] != stat_id(&info))
            {
                return Err(error("unowned conflicting account link kept"));
            }
            rustix::fs::unlinkat(&fd, leaf_name, rustix::fs::AtFlags::empty())
                .map_err(|_| error("cannot safely remove account link"))?;
        }
        if claude {
            // The identity is only known after creation: a kill in between
            // leaves an unrecorded link that uninstall will not touch.
            rustix::fs::symlinkat(&target, &fd, leaf_name)
                .map_err(|_| error("cannot safely create account link"))?;
            let made = rustix::fs::statat(&fd, leaf_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| error("cannot inspect account link"))?;
            rows[index]["links"]
                .as_object_mut()
                .ok_or_else(|| error("invalid receipt links"))?
                .insert(
                    (*name).to_owned(),
                    json!({"target":target,"identity":stat_id(&made)}),
                );
            save(rows)?;
        } else {
            rows[index]["links"]
                .as_object_mut()
                .ok_or_else(|| error("invalid receipt links"))?
                .insert((*name).to_owned(), json!(target));
            save(rows)?;
            rustix::fs::symlinkat(&target, &fd, leaf_name)
                .map_err(|_| error("cannot safely create account link"))?;
        }
    }
    if claude {
        write_locked(
            &claude_registry,
            &json_bytes(&next_claude_doc)?,
            false,
            Some(&claude_raw),
        )?;
    }
    write_locked(&registry, &json_bytes(&next_value)?, false, Some(&old))?;
    Ok(Output::text(format!(
        "{label} account {id}: registered, login pending\n"
    )))
}
fn home_placement(path: &Path, main: Option<&Path>) -> Result<()> {
    let main_homes = [
        ("CODEX_HOME", ".codex"),
        ("CLAUDE_CONFIG_DIR", ".claude"),
        ("PI_CODING_AGENT_DIR", ".pi/agent"),
    ];
    for (variable, fallback) in main_homes {
        if path == physical(&env_path(variable, home()?.join(fallback)))? {
            return Err(error("account home must differ from the main home"));
        }
    }
    if main == Some(path) {
        return Err(error("account home must differ from the main home"));
    }
    for ancestor in path.ancestors() {
        if ancestor.join(".git").exists()
            || (ancestor.join("HEAD").is_file()
                && ancestor.join("objects").is_dir()
                && ancestor.join("refs").is_dir())
        {
            return Err(error(
                "account home must not be inside a repository; use --home elsewhere",
            ));
        }
    }
    Ok(())
}
/// Antigravity rows live in sibling files: routing and peer launch refuse any
/// unproved provider key in `provider-accounts.json`, so AGY stays unrouted.
const AGY_REGISTRY: &str = "provider-accounts.antigravity.json";
const AGY_RECEIPT: &str = "antigravity-account-receipts.json";
/// gcloud's ADC file in a `CLOUDSDK_CONFIG`-style profile home; only its
/// metadata is inspected, never its bytes.
const AGY_PROFILE: &str = "application_default_credentials.json";
const AGY_UNVERIFIED: &str = "login and served identity unverified";
/// Register, verify or remove a local Antigravity ADC profile home. Records
/// bind the home's stat identity; nothing here proves login, the served
/// account or runtime routing, and removal never touches the home.
fn antigravity(args: &Args, id: &str) -> Result<Output> {
    account_flags(args, id)?;
    if args.has("--repair") {
        return Err(error("antigravity accounts have no links to repair"));
    }
    let config = account_config_dir()?;
    let _guard = lock(&config.join("provider-accounts.json"))?;
    let default_claude = physical(&home()?.join(".claude"))?;
    let host = AgyHost {
        config: &config,
        default_claude: &default_claude,
        placement: &|path| home_placement(path, None),
        mutate: &real_mutate,
        sync: &real_sync,
    };
    agy_locked(args, id, &host, || (), || ())
}
/// Environment-derived inputs, injected so unit tests never consult a real home.
struct AgyHost<'a> {
    config: &'a Path,
    default_claude: &'a Path,
    placement: &'a dyn Fn(&Path) -> Result<()>,
    /// The destination write; tests wrap it to inject publication outcomes.
    mutate: &'a AgyMutate<'a>,
    /// Parent-directory durability; tests count or fail it around the real helper.
    sync: &'a AgySync<'a>,
}
type AgySync<'a> =
    dyn Fn(&oms_safe_fs::Root, &Path) -> std::result::Result<(), oms_safe_fs::ByteError> + 'a;
fn real_sync(
    root: &oms_safe_fs::Root,
    path: &Path,
) -> std::result::Result<(), oms_safe_fs::ByteError> {
    root.sync_parent(path)
}
type AgyMutate<'a> = dyn Fn(
        &oms_safe_fs::Root,
        &Path,
        &[u8],
        Option<&[u8]>,
    ) -> std::result::Result<oms_safe_fs::MutationOutcome, oms_safe_fs::MutationError<Error>>
    + 'a;
/// A failed destination write. `before` is true only when the helper proved
/// no replacement was attempted; otherwise the new bytes may be visible.
struct WriteFail {
    before: bool,
    message: String,
}
/// The locked transaction; `before_publish` runs after every validation and
/// before the publication recheck, and `between_writes` runs after the first
/// atomic write succeeds and before the second, as test seams for races and
/// partial publication failures.
fn agy_locked(
    args: &Args,
    id: &str,
    host: &AgyHost<'_>,
    before_publish: impl FnOnce(),
    between_writes: impl FnOnce(),
) -> Result<Output> {
    if !host.config.is_absolute() {
        return Err(error("antigravity config root must be an absolute path"));
    }
    let config = physical(host.config)?;
    let root = private_root(&config)?;
    let registry_path = config.join(AGY_REGISTRY);
    let receipt_path = config.join(AGY_RECEIPT);
    let read_own = |path: &Path| -> Result<Option<Vec<u8>>> {
        let options = oms_safe_fs::ByteReadOptions {
            missing_ok: false,
            max_bytes: LIMIT,
        };
        match root.read_no_follow(path, options) {
            Err(oms_safe_fs::ByteError::Missing) => Ok(None),
            Err(_) => Err(error(
                "antigravity account files must be private, single-link and owned by the current user",
            )),
            Ok(raw) if raw.is_empty() => Err(empty_metadata("antigravity", path)),
            Ok(raw) => Ok(Some(raw)),
        }
    };
    let old_registry = read_own(&registry_path)?;
    let old_receipt = read_own(&receipt_path)?;
    let mut registry = match &old_registry {
        Some(raw) => json(raw)?,
        None => agy_empty_registry(),
    };
    let mut receipts = match &old_receipt {
        Some(raw) => json(raw)?,
        None => json!([]),
    };
    let (old_registry, old_receipt) = (old_registry.as_deref(), old_receipt.as_deref());
    validate_agy(&registry, &receipts)?;
    let rows = registry["accounts"].as_array_mut().unwrap();
    let receipts = receipts.as_array_mut().unwrap();
    if args.has("--remove") {
        if !rows.iter().chain(receipts.iter()).any(|r| r["id"] == id) {
            // An earlier attempt may have published the removal without a
            // confirmed directory sync; an existing metadata file proves the
            // physical parent to sync before calling it settled.
            if let Some(path) = [(&registry_path, old_registry), (&receipt_path, old_receipt)]
                .into_iter()
                .find_map(|(path, old)| old.map(|_| path))
            {
                (host.sync)(&root, path).map_err(|e| {
                    error(format!(
                        "directory durability unconfirmed: {e:?}; {id} is absent from the account files but the removal is not confirmed durable; rerun the same --remove"
                    ))
                })?;
            }
            return Ok(Output::text(format!(
                "Antigravity account {id} is not registered; nothing changed\n"
            )));
        }
        rows.retain(|r| r["id"] != id);
        receipts.retain(|r| r["id"] != id);
        let registry_written = write_private(host, &root, &registry_path, &registry, old_registry)
            .map_err(|f| {
                first_write_error(
                    f,
                    AGY_REGISTRY,
                    AGY_RECEIPT,
                    "home and ADC profile were not modified",
                    "--remove",
                )
            })?;
        between_writes();
        if let Err(err) = write_private(host, &root, &receipt_path, &json!(receipts), old_receipt) {
            if err.before {
                if let Some(written) = registry_written.as_deref() {
                    if rollback_private(host, &root, &registry_path, old_registry, written) {
                        return Err(error(format!(
                            "removal failed while writing {AGY_RECEIPT} ({}); rolled back {AGY_REGISTRY}; home and ADC profile were not modified",
                            err.message
                        )));
                    }
                    return Err(error(format!(
                        "partial publication failure: {AGY_REGISTRY} may have been updated, but writing {AGY_RECEIPT} failed ({}) and rollback was not confirmed; home and ADC profile were not modified; rerun --remove to finish cleanup",
                        err.message
                    )));
                }
                return Err(error(format!(
                    "removal failed while writing {AGY_RECEIPT} ({}); {AGY_REGISTRY} already held the removal and was not rolled back; home and ADC profile were not modified; rerun --remove to finish cleanup",
                    err.message
                )));
            }
            return Err(error(format!(
                "partial publication failure: {AGY_RECEIPT} write outcome is unconfirmed ({}); {AGY_REGISTRY} was not rolled back; home and ADC profile were not modified; rerun the same --remove to reconcile",
                err.message
            )));
        }
        return Ok(Output::text(format!(
            "Antigravity account removed: {id}; home and ADC profile kept\n"
        )));
    }
    let receipt_for = |rid: &Value, rhome: &Value| {
        receipts
            .iter()
            .find(|r| r["id"] == *rid && r["home"] == *rhome)
            .cloned()
    };
    if args.has("--verify") {
        if rows.is_empty() {
            return Err(error("Antigravity account is not registered"));
        }
        let mut text = String::new();
        let mut failed = false;
        for row in rows.iter() {
            let checked = receipt_for(&row["id"], &row["home"])
                .ok_or_else(|| error("missing antigravity account receipt"))
                .and_then(|receipt| {
                    let home = agy_home(Path::new(row["home"].as_str().unwrap()))?;
                    if agy_identity(&home.stat) != receipt["identity"] {
                        return Err(error("home identity no longer matches its receipt"));
                    }
                    agy_profile(home.profile.as_ref())
                });
            let detail = match checked {
                Ok(true) => "local profile structure verified".to_owned(),
                Ok(false) => "ADC profile missing".to_owned(),
                Err(e) => e.message,
            };
            let good = detail == "local profile structure verified";
            failed |= !good;
            text.push_str(&format!(
                "{}: antigravity account {} {detail}; {AGY_UNVERIFIED}\n",
                if good { "ok" } else { "warn" },
                row["id"].as_str().unwrap()
            ));
        }
        let mut out = Output::text(text);
        out.code = i32::from(failed);
        return Ok(out);
    }
    let existing = rows.iter().find(|r| r["id"] == id).cloned();
    for row in rows.iter() {
        if row["id"] == id {
            continue;
        }
        if !receipts
            .iter()
            .any(|r| r["id"] == row["id"] && r["home"] == row["home"])
        {
            return Err(error(format!(
                "missing or mismatched antigravity account receipt for {} ({AGY_RECEIPT}); remove and re-register that account first; nothing was changed",
                row["id"].as_str().unwrap()
            )));
        }
    }
    let path = match (args.get("--home"), &existing) {
        (Some(raw), _) => PathBuf::from(raw),
        (None, Some(row)) => PathBuf::from(row["home"].as_str().unwrap()),
        (None, None) => return Err(error("antigravity accounts require --home PATH")),
    };
    if !path.is_absolute() {
        return Err(error(
            "antigravity account home must be an absolute physical path",
        ));
    }
    let home = agy_home(&path)?;
    (host.placement)(&path)?;
    let identity = agy_identity(&home.stat);
    let same = |home: &Path| {
        use std::os::unix::fs::MetadataExt;
        home == path
            || std::fs::metadata(home).is_ok_and(|m| {
                json!(m.dev()) == identity["st_dev"] && json!(m.ino()) == identity["st_ino"]
            })
    };
    if foreign_homes(&config, host.default_claude)?
        .iter()
        .any(|h| same(h))
    {
        return Err(error(
            "canonical home is already registered by another provider",
        ));
    }
    let home_value = json!(path);
    for row in rows.iter().chain(receipts.iter()) {
        let other = Path::new(row["home"].as_str().unwrap());
        if row["id"] == id && row["home"] != home_value {
            return Err(error(format!(
                "antigravity account {id} is already registered with another home; nothing was changed"
            )));
        }
        if row["id"] != id && same(other) {
            return Err(error("duplicate provider account id or canonical home"));
        }
    }
    match receipt_for(&json!(id), &home_value) {
        Some(receipt) if receipt["identity"] != identity => {
            return Err(error(
                "antigravity account receipt does not match the home identity; nothing was changed",
            ));
        }
        None if existing.is_some() => {
            return Err(error(
                "missing antigravity account receipt; remove the account and register it again",
            ));
        }
        Some(_) => (),
        None => receipts.push(json!({"id":id,"home":path,"identity":identity,
            "verification":"unverified"})),
    }
    let profile = agy_profile(home.profile.as_ref())?;
    let entry = json!({"id":id,"home":path});
    if existing.is_none() || args.has("--first") || args.has("--last") {
        rows.retain(|r| r["id"] != id);
        if args.has("--first") {
            rows.insert(0, entry);
        } else {
            rows.push(entry);
        }
    }
    before_publish();
    // Publication goes through the descriptors held since validation: the
    // home's ancestry and leaf are re-stat'ed, and both writes use `root`.
    agy_unchanged(&home, &identity, true)?;
    let receipt_written = write_private(host, &root, &receipt_path, &json!(receipts), old_receipt)
        .map_err(|f| {
            first_write_error(
                f,
                AGY_RECEIPT,
                AGY_REGISTRY,
                "ADC profile was not modified",
                "registration",
            )
        })?;
    between_writes();
    let second = agy_unchanged(&home, &identity, false)
        .map_err(|e| WriteFail {
            before: true,
            message: e.message,
        })
        .and_then(|()| {
            write_private(host, &root, &registry_path, &registry, old_registry).map(|_| ())
        });
    if let Err(err) = second {
        if !err.before {
            return Err(error(format!(
                "partial publication failure: {AGY_RECEIPT} was written, but {AGY_REGISTRY} write outcome is unconfirmed ({}); {AGY_RECEIPT} was not rolled back; ADC profile was not modified; rerun the same registration or --remove to reconcile",
                err.message
            )));
        }
        if let Some(written) = receipt_written.as_deref() {
            if rollback_private(host, &root, &receipt_path, old_receipt, written) {
                return Err(error(format!(
                    "publication failed before {AGY_REGISTRY} was updated ({}); rolled back {AGY_RECEIPT}; ADC profile was not modified",
                    err.message
                )));
            }
            return Err(error(format!(
                "partial publication failure: {AGY_RECEIPT} may have been written, but {AGY_REGISTRY} was not updated ({}) and rollback was not confirmed; ADC profile was not modified; rerun the same registration or --remove to recover",
                err.message
            )));
        }
        return Err(error(format!(
            "publication failed before {AGY_REGISTRY} was updated ({}); {AGY_RECEIPT} already held this registration and was not rolled back; ADC profile was not modified; rerun the same registration or --remove to recover",
            err.message
        )));
    }
    Ok(Output::text(format!(
        "Antigravity account {id}: registered; ADC profile {}; {AGY_UNVERIFIED}\n",
        if profile { "present" } else { "pending" }
    )))
}
fn validate_agy(registry: &Value, receipts: &Value) -> Result<()> {
    let bad = || error("invalid antigravity account registry or receipt");
    let keys = |v: &Value, allowed: &[&str]| {
        v.as_object()
            .is_some_and(|m| m.keys().all(|k| allowed.contains(&k.as_str())))
    };
    if registry["schema"] != 1
        || registry["provider"] != "antigravity"
        || !keys(registry, &["schema", "provider", "accounts"])
    {
        return Err(bad());
    }
    let rows = registry["accounts"].as_array().ok_or_else(bad)?;
    let receipts = receipts.as_array().ok_or_else(bad)?;
    let stat_keys = ["st_uid", "st_dev", "st_ino", "st_mode"];
    for (list, receipt) in [(rows, false), (receipts, true)] {
        let mut ids = std::collections::BTreeSet::new();
        let mut homes = std::collections::BTreeSet::new();
        for row in list {
            let (rid, rhome) = if receipt {
                if !keys(row, &["id", "home", "identity", "verification"])
                    || row["verification"] != "unverified"
                    || !keys(&row["identity"], &stat_keys)
                    || !stat_keys.iter().all(|k| row["identity"][k].is_u64())
                {
                    return Err(bad());
                }
                let probe = json!({"id":row["id"],"home":row["home"]});
                let (rid, rhome) = validate_account_row(&probe, false).map_err(|_| bad())?;
                (rid.to_owned(), rhome.to_owned())
            } else {
                let (rid, rhome) = validate_account_row(row, false).map_err(|_| bad())?;
                (rid.to_owned(), rhome.to_owned())
            };
            if !ids.insert(rid) || !homes.insert(rhome) {
                return Err(error("duplicate provider account id or canonical home"));
            }
        }
    }
    Ok(())
}
/// Private-registry root at the nearest existing ancestor, as `write_locked`
/// anchors, so a missing config directory is created beneath safe ancestry.
fn private_root(dir: &Path) -> Result<oms_safe_fs::Root> {
    let mut anchor = dir;
    while !anchor.is_dir() {
        anchor = anchor
            .parent()
            .ok_or_else(|| error("missing destination root"))?;
    }
    oms_safe_fs::Root::open_private_registry(anchor).map_err(|_| {
        error(
            "unsafe antigravity account registry ancestry; every directory from / must be physical, user- or root-owned and not group/world-writable",
        )
    })
}
fn real_mutate(
    root: &oms_safe_fs::Root,
    path: &Path,
    bytes: &[u8],
    old: Option<&[u8]>,
) -> std::result::Result<oms_safe_fs::MutationOutcome, oms_safe_fs::MutationError<Error>> {
    root.atomic_mutate(
        path,
        oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: LIMIT,
            },
            create_parent: true,
            expected_exists: Some(old.is_some()),
            ..Default::default()
        },
        |current| {
            if current != old.unwrap_or_default() {
                return Err(error("destination changed during transaction"));
            }
            Ok(bytes.to_vec())
        },
    )
}
/// `old` is `None` only when the file was absent (ENOENT); a file appearing
/// or changing since that read refuses the write. Returns `Some(bytes)` when
/// the file was written, or `None` when its contents were already identical
/// and the directory entry was re-synced (a prior retry may have published
/// without confirming durability).
fn write_private(
    host: &AgyHost<'_>,
    root: &oms_safe_fs::Root,
    path: &Path,
    value: &Value,
    old: Option<&[u8]>,
) -> std::result::Result<Option<Vec<u8>>, WriteFail> {
    use oms_safe_fs::{MutationDisposition, MutationError, MutationPhase};
    let bytes = json_bytes(value).map_err(|e| WriteFail {
        before: true,
        message: e.message,
    })?;
    match (host.mutate)(root, path, &bytes, old) {
        Ok(out) if out.disposition == MutationDisposition::Unchanged => (host.sync)(root, path)
            .map(|()| None)
            .map_err(|e| WriteFail {
                before: false,
                message: format!("directory durability unconfirmed: {e:?}"),
            }),
        Ok(_) => Ok(Some(bytes)),
        Err(MutationError::Transform(e)) => Err(WriteFail {
            before: true,
            message: e.message,
        }),
        Err(MutationError::Storage { phase, error }) => Err(WriteFail {
            before: phase == MutationPhase::BeforePublication,
            message: format!("{phase:?}: {error:?}"),
        }),
    }
}
/// A first-write failure: nothing else was written, so only the outcome of
/// this one file needs an honest recovery instruction.
fn first_write_error(f: WriteFail, file: &str, sibling: &str, kept: &str, verb: &str) -> Error {
    if f.before {
        return error(format!("atomic write refused: {}", f.message));
    }
    error(format!(
        "partial publication failure: {file} write outcome is unconfirmed ({}); {sibling} was not written; {kept}; rerun the same {verb} to reconcile",
        f.message
    ))
}
/// Best-effort rollback of a first atomic write when the second write is
/// proven not to have been attempted. Restores `previous` only if `path`
/// still holds `written` (never deletes files, never touches any ADC profile).
fn rollback_private(
    host: &AgyHost<'_>,
    root: &oms_safe_fs::Root,
    path: &Path,
    previous: Option<&[u8]>,
    written: &[u8],
) -> bool {
    let Some(prev) = previous else {
        return false;
    };
    (host.mutate)(root, path, prev, Some(written)).is_ok()
}
fn agy_empty_registry() -> Value {
    json!({"schema":1,"provider":"antigravity","accounts":[]})
}
fn empty_metadata(owner: &str, path: &Path) -> Error {
    error(format!(
        "{owner} account metadata is empty (corrupt): {}; nothing was changed",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}
/// Existing account metadata read strictly: only ENOENT is missing, and an
/// existing empty file is corruption rather than a fresh registry.
fn present(path: &Path, owner: &str) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(_) => match read(path, LIMIT)? {
            raw if raw.is_empty() => Err(empty_metadata(owner, path)),
            raw => Ok(Some(raw)),
        },
    }
}
fn invalid_metadata(file: &str, e: Error) -> Error {
    error(format!(
        "invalid provider account metadata ({file}): {}; nothing was changed",
        e.message
    ))
}
/// Rows of a codex/pi link receipt (array) or Claude effects document, held
/// to the shape their writers in `account` create.
fn link_receipts<'a>(doc: &'a Value, claude: bool, names: &[&str]) -> Result<&'a Vec<Value>> {
    let bad = || error("invalid provider account receipt");
    let rows = if claude {
        if doc["schema"] != 1
            || !doc.as_object().is_some_and(|m| {
                m.keys()
                    .all(|k| ["schema", "accounts"].contains(&k.as_str()))
            })
        {
            return Err(bad());
        }
        doc["accounts"].as_array()
    } else {
        doc.as_array()
    }
    .ok_or_else(bad)?;
    let (keys, strings, stats): (&[&str], &[&str], &[&str]) = if claude {
        (
            &[
                "id",
                "provider",
                "home",
                "parent",
                "parent_identity",
                "identity",
                "settings",
                "hooks_receipt",
                "source_root",
                "status",
                "links",
            ],
            &[
                "provider",
                "parent",
                "settings",
                "hooks_receipt",
                "source_root",
                "status",
            ],
            &["parent_identity", "identity"],
        )
    } else {
        (&["id", "home", "links", "st_dev", "st_ino"], &[], &[])
    };
    for row in rows {
        let obj = row.as_object().ok_or_else(bad)?;
        if !obj.keys().all(|k| keys.contains(&k.as_str()))
            || strings
                .iter()
                .any(|k| obj.get(*k).is_some_and(|v| !v.is_string()))
            || stats.iter().any(|k| {
                obj.get(*k)
                    .is_some_and(|v| !v.as_object().is_some_and(|m| m.values().all(Value::is_u64)))
            })
            || ["st_dev", "st_ino"]
                .iter()
                .any(|k| obj.get(*k).is_some_and(|v| !v.is_u64()))
            || obj
                .get("provider")
                .is_some_and(|p| p.as_str() != Some("claude"))
        {
            return Err(bad());
        }
        validate_account_row(&json!({"id":row["id"],"home":row["home"]}), false)?;
        validate_receipt_links(row, names, claude)?;
    }
    Ok(rows)
}
/// Every other provider's registered, seeded or receipted home. Foreign
/// metadata refuses, never skips, when it is empty, fails its owner's schema,
/// or a non-default registered row lacks the receipt its owner writes first.
fn foreign_homes(config: &Path, default_claude: &Path) -> Result<Vec<PathBuf>> {
    let load = |file: &str| -> Result<Option<Value>> {
        present(&config.join(file), "provider")?
            .map(|raw| json(&raw).map_err(|e| invalid_metadata(file, e)))
            .transpose()
    };
    let shared =
        load("provider-accounts.json")?.unwrap_or_else(|| json!({"schema":1,"providers":{}}));
    let shared_homes = validate_shared_registry(&shared)
        .map_err(|e| invalid_metadata("provider-accounts.json", e))?;
    let claude = load("provider-accounts.claude.json")?;
    if let Some(doc) = &claude {
        validate_claude_registry(doc, default_claude, &shared, &shared_homes)
            .map_err(|e| invalid_metadata("provider-accounts.claude.json", e))?;
    }
    let mut homes: Vec<PathBuf> = shared_homes.into_iter().collect();
    if shared["providers"].get("claude").is_none()
        && claude
            .as_ref()
            .and_then(|doc| doc["accounts"].as_array())
            .is_some_and(|accounts| !accounts.is_empty())
    {
        homes.push(physical(default_claude)?);
    }
    let mut owned = std::collections::BTreeMap::new();
    for (owner, file, names) in [
        ("codex", "codex-account-links.json", SHARED),
        ("pi", "pi-account-links.json", PI_SHARED),
        ("claude", "provider-account-effects.json", CLAUDE_SHARED),
    ] {
        let mut receipted = Vec::new();
        if let Some(doc) = load(file)? {
            for row in link_receipts(&doc, owner == "claude", names)
                .map_err(|e| invalid_metadata(file, e))?
            {
                let home = physical(Path::new(row["home"].as_str().unwrap()))?;
                receipted.push((row["id"].clone(), home.clone()));
                homes.push(home);
            }
        }
        owned.insert(owner, (file, receipted));
    }
    let registered = shared["providers"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(owner, s)| {
            s["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |row| (owner.as_str(), row))
        })
        .chain(claude.iter().flat_map(|doc| {
            doc["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| ("claude", row))
        }));
    for (owner, row) in registered {
        if row["id"] == "default" {
            continue;
        }
        let home = physical(Path::new(row["home"].as_str().unwrap()))?;
        let (file, receipted) = &owned[owner];
        if !receipted
            .iter()
            .any(|(rid, rhome)| *rid == row["id"] && *rhome == home)
        {
            return Err(error(format!(
                "missing {owner} account receipt for {} ({file}); re-register or --repair that account first; nothing was changed",
                row["id"].as_str().unwrap()
            )));
        }
        homes.push(home);
    }
    Ok(homes)
}
/// Antigravity homes as foreign metadata for codex, claude and pi
/// registration, refused on the same terms as `foreign_homes`.
fn agy_homes(config: &Path) -> Result<Vec<PathBuf>> {
    let invalid = |e| invalid_metadata(&format!("{AGY_REGISTRY}, {AGY_RECEIPT}"), e);
    let load = |file: &str| -> Result<Option<Value>> {
        present(&config.join(file), "antigravity")?
            .map(|raw| json(&raw).map_err(invalid))
            .transpose()
    };
    let registry = load(AGY_REGISTRY)?.unwrap_or_else(agy_empty_registry);
    let receipts = load(AGY_RECEIPT)?.unwrap_or_else(|| json!([]));
    validate_agy(&registry, &receipts).map_err(invalid)?;
    let (rows, receipts) = (
        registry["accounts"].as_array().unwrap(),
        receipts.as_array().unwrap(),
    );
    if let Some(row) = rows.iter().find(|row| {
        !receipts
            .iter()
            .any(|r| r["id"] == row["id"] && r["home"] == row["home"])
    }) {
        return Err(error(format!(
            "missing antigravity account receipt for {} ({AGY_RECEIPT}); nothing was changed",
            row["id"].as_str().unwrap()
        )));
    }
    rows.iter()
        .chain(receipts)
        .map(|row| physical(Path::new(row["home"].as_str().unwrap())))
        .collect()
}
/// A profile home validated through private ancestry. `root` holds the
/// ancestor and home descriptors until publication; `profile` is the ADC
/// file's no-follow metadata (its bytes are never read).
struct AgyHome {
    root: oms_safe_fs::Root,
    stat: rustix::fs::Stat,
    profile: Option<rustix::fs::Stat>,
}
fn agy_private_dir(info: &rustix::fs::Stat) -> bool {
    info.st_uid == rustix::process::getuid().as_raw() && info.st_mode & 0o7077 == 0
}
/// The home must be named by its physical spelling through private ancestry
/// and be an owner-only directory.
fn agy_home(path: &Path) -> Result<AgyHome> {
    let root = oms_safe_fs::Root::open_private_registry(path).map_err(|_| {
        error(
            "antigravity account home must be an existing physical directory (no symlink, `.` or `..`) whose ancestors are user- or root-owned and not group/world-writable",
        )
    })?;
    let (stat, profile) = root
        .pinned_metadata(std::ffi::OsStr::new(AGY_PROFILE))
        .map_err(|_| error("cannot inspect account directory"))?;
    if !agy_private_dir(&stat) {
        return Err(error(
            "antigravity account home must be owned by the current user and private (chmod 700)",
        ));
    }
    Ok(AgyHome {
        root,
        stat,
        profile,
    })
}
/// Publication recheck through the held descriptors, never a reopened name:
/// the ancestry and home leaf still match their validated no-follow stats,
/// the home keeps its private mode and recorded identity, and the ADC
/// profile keeps its validated presence, identity, mode and link count.
fn agy_unchanged(home: &AgyHome, identity: &Value, before_write: bool) -> Result<()> {
    let suffix = if before_write {
        "; nothing was changed"
    } else {
        ""
    };
    let changed = || {
        error(format!(
            "antigravity account home changed during registration{suffix}"
        ))
    };
    let (stat, profile) = home
        .root
        .pinned_metadata(std::ffi::OsStr::new(AGY_PROFILE))
        .map_err(|_| changed())?;
    if !agy_private_dir(&stat) || agy_identity(&stat) != *identity {
        return Err(changed());
    }
    let key = |s: &rustix::fs::Stat| (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_nlink);
    let same = match (&home.profile, &profile) {
        (None, None) => true,
        (Some(a), Some(b)) => agy_profile(Some(b)).is_ok() && key(a) == key(b),
        _ => false,
    };
    if !same {
        return Err(error(format!(
            "antigravity ADC profile changed during registration{suffix}"
        )));
    }
    Ok(())
}
fn agy_identity(info: &rustix::fs::Stat) -> Value {
    let mut identity = stat_id(info);
    identity["st_mode"] = json!(info.st_mode);
    identity
}
/// Present and structurally safe (`true`), absent (`false`), or refused.
fn agy_profile(profile: Option<&rustix::fs::Stat>) -> Result<bool> {
    match profile {
        None => Ok(false),
        Some(s)
            if rustix::fs::FileType::from_raw_mode(s.st_mode)
                == rustix::fs::FileType::RegularFile
                && s.st_nlink == 1
                && s.st_uid == rustix::process::getuid().as_raw()
                && s.st_mode & 0o7077 == 0 =>
        {
            Ok(true)
        }
        Some(_) => Err(error(
            "antigravity ADC profile must be a private (0600), single-link regular file owned by the current user",
        )),
    }
}
fn validate_receipt_links(
    row: &Value,
    names: &[&str],
    claude: bool,
) -> Result<serde_json::Map<String, Value>> {
    let links = row["links"]
        .as_object()
        .ok_or_else(|| error("invalid receipt links"))?
        .clone();
    for (name, recorded) in &links {
        if !names.contains(&name.as_str()) {
            return Err(error("invalid recorded account link"));
        }
        if (if claude {
            recorded["target"].as_str()
        } else {
            recorded.as_str()
        })
        .is_none()
        {
            return Err(error("invalid recorded link target"));
        }
    }
    Ok(links)
}
const CODEX_LOGIN_OVERRIDES: &[&str] = &[
    concat!("CODEX_API_", "KEY"),
    concat!("CODEX_ACCESS_", "TOKEN"),
    concat!("OPENAI_API_", "KEY"),
    "OPENAI_BASE_URL",
];
const CLAUDE_LOGIN_OVERRIDES: &[&str] = &[
    concat!("ANTHROPIC_API_", "KEY"),
    concat!("ANTHROPIC_AUTH_", "TOKEN"),
    concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
];
fn login_overrides(provider: &str) -> &'static [&'static str] {
    match provider {
        "codex" => CODEX_LOGIN_OVERRIDES,
        "claude" => CLAUDE_LOGIN_OVERRIDES,
        _ => &[],
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct LoginTarget {
    provider: String,
    account_id: Option<String>,
}
fn parse_login_spec(raw: &str) -> Result<LoginTarget> {
    let (provider, account_id) = match raw.split_once(':') {
        Some((p, id)) => (p, Some(id)),
        None => (raw, None),
    };
    match provider {
        "codex" | "claude" => {
            if let Some(id) = account_id
                && !valid_account_id(id)
            {
                return Err(error("invalid provider account id"));
            }
            Ok(LoginTarget {
                provider: provider.to_owned(),
                account_id: account_id.map(str::to_owned),
            })
        }
        "antigravity" => {
            if account_id.is_some() {
                return Err(error(
                    "antigravity does not support per-account sign-in; use --login antigravity",
                ));
            }
            Ok(LoginTarget {
                provider: "antigravity".to_owned(),
                account_id: None,
            })
        }
        _ => Err(error(
            "unsupported login provider; expected codex, claude, or antigravity",
        )),
    }
}
fn registered_provider_accounts(provider: &str) -> Result<Vec<(String, PathBuf)>> {
    if !["codex", "claude"].contains(&provider) {
        return Ok(Vec::new());
    }
    let config = account_config_dir()?;
    let registry = config.join("provider-accounts.json");
    let raw = optional(&registry)?;
    let shared = if raw.is_empty() {
        json!({"schema":1,"providers":{}})
    } else {
        json(&raw)?
    };
    let shared_homes = validate_shared_registry(&shared)?;
    if let Some(entries) = shared["providers"][provider]["accounts"].as_array() {
        let mut out = Vec::with_capacity(entries.len());
        for row in entries {
            let (id, home_str) = validate_account_row(row, true)?;
            out.push((id.to_owned(), physical(Path::new(home_str))?));
        }
        return Ok(out);
    }
    if provider == "claude" {
        let claude_registry = config.join("provider-accounts.claude.json");
        let claude_raw = optional(&claude_registry)?;
        if !claude_raw.is_empty() {
            let default_claude = physical(&home()?.join(".claude"))?;
            let claude_doc = json(&claude_raw)?;
            validate_claude_registry(&claude_doc, &default_claude, &shared, &shared_homes)?;
            if let Some(entries) = claude_doc["accounts"].as_array()
                && !entries.is_empty()
            {
                let mut out = vec![("default".to_owned(), default_claude)];
                for row in entries {
                    let (id, home_str) = validate_account_row(row, false)?;
                    out.push((id.to_owned(), physical(Path::new(home_str))?));
                }
                return Ok(out);
            }
        }
    }
    Ok(Vec::new())
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct LoginEntry {
    provider: String,
    account_id: Option<String>,
    home_env: Option<(&'static str, PathBuf)>,
}
impl LoginEntry {
    fn label(&self) -> String {
        match &self.account_id {
            Some(id) => format!("{}:{id}", self.provider),
            None => self.provider.clone(),
        }
    }
}
const LOGIN_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const LOGIN_UNKNOWN_CODE: i32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoginStatus {
    SignedIn,
    NotSignedIn,
    Unknown(String),
}
impl LoginStatus {
    fn exit_code(&self) -> i32 {
        match self {
            Self::SignedIn => 0,
            Self::NotSignedIn => 1,
            Self::Unknown(_) => LOGIN_UNKNOWN_CODE,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::SignedIn => "signed in".to_owned(),
            Self::NotSignedIn => "not signed in".to_owned(),
            Self::Unknown(reason) => format!("unknown: {reason}"),
        }
    }
}

fn resolve_login_entry(target: &LoginTarget) -> Result<LoginEntry> {
    match target.provider.as_str() {
        "codex" | "claude" => {
            let env_key = if target.provider == "codex" {
                "CODEX_HOME"
            } else {
                "CLAUDE_CONFIG_DIR"
            };
            if let Some(id) = &target.account_id {
                let accounts = registered_provider_accounts(&target.provider)?;
                let (_, path) = accounts
                    .into_iter()
                    .find(|(rid, _)| rid == id)
                    .ok_or_else(|| error(format!("unknown {} account: {id}", target.provider)))?;
                if id == "default" {
                    Ok(LoginEntry {
                        provider: target.provider.clone(),
                        account_id: None,
                        home_env: None,
                    })
                } else {
                    private_home(&path)?;
                    Ok(LoginEntry {
                        provider: target.provider.clone(),
                        account_id: Some(id.clone()),
                        home_env: Some((env_key, path)),
                    })
                }
            } else {
                Ok(LoginEntry {
                    provider: target.provider.clone(),
                    account_id: None,
                    home_env: None,
                })
            }
        }
        "antigravity" => Ok(LoginEntry {
            provider: "antigravity".to_owned(),
            account_id: None,
            home_env: None,
        }),
        _ => Err(error(
            "unsupported login provider; expected codex, claude, or antigravity",
        )),
    }
}
fn provider_login_spec(provider: &str) -> Result<(&'static str, &'static [&'static str])> {
    match provider {
        "codex" => Ok(("codex", &["login"])),
        "claude" => Ok(("claude", &["auth", "login"])),
        "antigravity" => Ok(("agy", &[])),
        _ => Err(error(
            "unsupported login provider; expected codex, claude, or antigravity",
        )),
    }
}
fn run_login_entry(entry: &LoginEntry, ctx: &Context<'_>) -> Result<(i32, String)> {
    use std::io::Write;
    let (binary, subargs) = provider_login_spec(&entry.provider)?;
    if entry.account_id.is_some()
        && let Some((_, home_path)) = &entry.home_env
    {
        private_home(home_path)?;
    }
    if !installed(binary) {
        return Err(error(format!("{binary} is not installed or not on PATH")));
    }
    if entry.provider == "antigravity" {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(
            stdout,
            "Sign in inside Antigravity ({binary}), then exit when done."
        );
        let _ = stdout.flush();
    }
    let mut command = Command::new(binary);
    command
        .args(subargs)
        .current_dir(ctx.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some((env_key, home_path)) = &entry.home_env {
        command.env(env_key, home_path);
    }
    for var in login_overrides(&entry.provider) {
        command.env_remove(var);
    }
    let status = match command.status() {
        Ok(status) => status,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(error(format!("{binary} is not installed or not on PATH")));
        }
        Err(_) => return Err(error(format!("failed to run {binary}"))),
    };
    let label = entry.label();
    if entry.provider == "antigravity" {
        let code = if status.success() {
            0
        } else {
            status.code().unwrap_or(1)
        };
        return Ok((code, format!("{label}: sign-in not verifiable\n")));
    }
    let home_path = entry.home_env.as_ref().map(|(_, path)| path.as_path());
    let verified = login_status(&entry.provider, home_path, ctx)?;
    let report = format!("{label}: {}\n", verified.label());
    Ok((verified.exit_code(), report))
}
fn login_direct(raw: &str, ctx: &Context<'_>) -> Result<Output> {
    let target = parse_login_spec(raw)?;
    let entry = resolve_login_entry(&target)?;
    let (code, report) = run_login_entry(&entry, ctx)?;
    Ok(Output {
        code,
        out: report.into_bytes(),
        err: Vec::new(),
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct MenuItem {
    entry: LoginEntry,
    status: String,
}
fn build_login_menu(ctx: &Context<'_>) -> Result<Vec<MenuItem>> {
    build_login_menu_with(registered_provider_accounts, installed, |provider, home| {
        login_status(provider, home, ctx)
    })
}
fn build_login_menu_with(
    mut accounts_for: impl FnMut(&str) -> Result<Vec<(String, PathBuf)>>,
    mut has_binary: impl FnMut(&str) -> bool,
    mut status_for: impl FnMut(&str, Option<&Path>) -> Result<LoginStatus>,
) -> Result<Vec<MenuItem>> {
    let mut items = Vec::new();
    for provider in ["codex", "claude"] {
        let (binary, _) = provider_login_spec(provider)?;
        let env_key = if provider == "codex" {
            "CODEX_HOME"
        } else {
            "CLAUDE_CONFIG_DIR"
        };
        let installed_bin = has_binary(binary);
        let accounts = accounts_for(provider)?;
        let rows: Vec<(Option<String>, Option<PathBuf>)> = if accounts.is_empty() {
            vec![(None, None)]
        } else {
            accounts
                .into_iter()
                .map(|(id, path)| {
                    if id == "default" {
                        (None, None)
                    } else {
                        (Some(id), Some(path))
                    }
                })
                .collect()
        };
        for (account_id, home_path) in rows {
            let status = if !installed_bin {
                format!("{binary} not installed")
            } else if let Some(path) = home_path.as_deref()
                && private_home(path).is_err()
            {
                LoginStatus::Unknown("unsafe or missing home".to_owned()).label()
            } else {
                status_for(provider, home_path.as_deref())?.label()
            };
            items.push(MenuItem {
                entry: LoginEntry {
                    provider: provider.to_owned(),
                    account_id,
                    home_env: home_path.map(|path| (env_key, path)),
                },
                status,
            });
        }
    }
    let agy_status = if has_binary("agy") {
        "sign-in not verifiable".to_owned()
    } else {
        "agy not installed".to_owned()
    };
    items.push(MenuItem {
        entry: LoginEntry {
            provider: "antigravity".to_owned(),
            account_id: None,
            home_env: None,
        },
        status: agy_status,
    });
    Ok(items)
}
fn format_login_menu(items: &[MenuItem]) -> String {
    let mut out = String::from("Provider sign-in:\n");
    for (idx, item) in items.iter().enumerate() {
        let label = match &item.entry.account_id {
            Some(id) => format!("{}:{id}", item.entry.provider),
            None if item.entry.provider == "antigravity" => "antigravity".to_owned(),
            None => format!("{} (default)", item.entry.provider),
        };
        out.push_str(&format!("  {}. {:<24} [{}]\n", idx + 1, label, item.status));
    }
    out
}
fn parse_menu_choice(input: &str, count: usize) -> Result<Option<usize>> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("q") {
        return Ok(None);
    }
    let choice: usize = trimmed
        .parse()
        .map_err(|_| error(format!("invalid selection: expected 1-{count} or q")))?;
    if choice == 0 || choice > count {
        return Err(error(format!("invalid selection: expected 1-{count} or q")));
    }
    Ok(Some(choice - 1))
}
fn wait_for_enter() {
    use std::io::{BufRead, Write};
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\nPress Enter to close.");
    let _ = stdout.flush();
    drop(stdout);
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
fn login_menu(ctx: &Context<'_>) -> Result<Output> {
    use std::io::{BufRead, Write};
    if !std::io::stdin().is_terminal() {
        return Err(error("--login-menu requires an interactive terminal"));
    }
    let items = match build_login_menu(ctx) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("error: {}", e.message);
            wait_for_enter();
            return Ok(Output {
                code: e.code,
                out: Vec::new(),
                err: Vec::new(),
            });
        }
    };
    {
        let mut stdout = std::io::stdout().lock();
        let _ = write!(
            stdout,
            "{}\nChoose 1-{} (or q to cancel): ",
            format_login_menu(&items),
            items.len()
        );
        let _ = stdout.flush();
    }
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    if read == 0 {
        return Ok(Output {
            code: 0,
            out: Vec::new(),
            err: Vec::new(),
        });
    }
    let selected = match parse_menu_choice(&line, items.len()) {
        Ok(Some(idx)) => &items[idx].entry,
        Ok(None) => {
            return Ok(Output {
                code: 0,
                out: Vec::new(),
                err: Vec::new(),
            });
        }
        Err(e) => {
            eprintln!("error: {}", e.message);
            wait_for_enter();
            return Ok(Output {
                code: e.code,
                out: Vec::new(),
                err: Vec::new(),
            });
        }
    };
    match run_login_entry(selected, ctx) {
        Ok((code, report)) => {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{report}");
            let _ = stdout.flush();
            drop(stdout);
            wait_for_enter();
            Ok(Output {
                code,
                out: Vec::new(),
                err: Vec::new(),
            })
        }
        Err(e) => {
            eprintln!("error: {}", e.message);
            wait_for_enter();
            Ok(Output {
                code: e.code,
                out: Vec::new(),
                err: Vec::new(),
            })
        }
    }
}
fn login_status(provider: &str, home: Option<&Path>, ctx: &Context<'_>) -> Result<LoginStatus> {
    login_status_with_timeout(provider, home, ctx, LOGIN_STATUS_TIMEOUT)
}
fn login_status_with_timeout(
    provider: &str,
    home: Option<&Path>,
    ctx: &Context<'_>,
    timeout: std::time::Duration,
) -> Result<LoginStatus> {
    let (binary, args, env): (&str, &[&str], &str) = match provider {
        "codex" => ("codex", &["login", "status"], "CODEX_HOME"),
        "claude" => ("claude", &["auth", "status"], "CLAUDE_CONFIG_DIR"),
        _ => return Ok(LoginStatus::Unknown("unsupported provider".to_owned())),
    };
    let mut command = Command::new(binary);
    command.args(args).stdin(Stdio::null());
    if let Some(path) = home {
        command.env(env, path);
    }
    for var in login_overrides(provider) {
        command.env_remove(var);
    }
    run_login_status_command(command, binary, ctx.interrupted, timeout)
}
fn run_login_status_command(
    command: Command,
    binary: &str,
    interrupted: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
) -> Result<LoginStatus> {
    if interrupted.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(LoginStatus::Unknown("status check interrupted".to_owned()));
    }
    let out = capture_timeout(command, interrupted, timeout)?;
    Ok(match out.code {
        0 => LoginStatus::SignedIn,
        124 => LoginStatus::Unknown("status check timed out".to_owned()),
        130 => LoginStatus::Unknown("status check interrupted".to_owned()),
        127 => LoginStatus::Unknown(format!("{binary} not installed")),
        _ => LoginStatus::NotSignedIn,
    })
}
fn stat_id(info: &rustix::fs::Stat) -> Value {
    json!({"st_uid":info.st_uid,"st_dev":info.st_dev,"st_ino":info.st_ino})
}
fn path_id(path: &Path) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let info = std::fs::symlink_metadata(path)?;
    Ok(json!({"st_uid":info.uid(),"st_dev":info.dev(),"st_ino":info.ino()}))
}
/// Descriptor and leaf for a link name; `dir/leaf` names resolve through a
/// private, non-symlink subdirectory that is created only on request.
fn leaf<'a>(
    directory: &std::os::fd::OwnedFd,
    name: &'a str,
    create: bool,
) -> Result<Option<(std::os::fd::OwnedFd, &'a str)>> {
    let Some((sub, file)) = name.split_once('/') else {
        return Ok(Some((directory.try_clone()?, name)));
    };
    if create {
        match rustix::fs::mkdirat(directory, sub, rustix::fs::Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(_) => return Err(error("cannot create account subdirectory")),
        }
    }
    let fd = match rustix::fs::openat(
        directory,
        sub,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
        Err(_) => {
            return Err(error(
                "cannot open account subdirectory; existing entries and receipts kept; remove or replace conflicting path",
            ));
        }
    };
    let info = rustix::fs::fstat(&fd).map_err(|_| {
        error("cannot inspect account subdirectory; existing entries and receipts kept")
    })?;
    if info.st_uid != rustix::process::getuid().as_raw() || info.st_mode & 0o022 != 0 {
        return Err(error(
            "account subdirectory must be private; existing entries and receipts kept; run chmod 700 on the subdirectory",
        ));
    }
    Ok(Some((fd, file)))
}
const CLAUDE_SHARED: &[&str] = &["CLAUDE.md", "skills", "output-styles/oms-korean.md"];
const PI_SHARED: &[&str] = &["AGENTS.md", "skills"];
const SHARED: &[&str] = &[
    "AGENTS.md",
    "config.toml",
    "hooks.json",
    ".hook-trust",
    "plugins",
    "rules",
    "skills",
];
fn physical(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().ok_or_else(|| error("invalid home path"))?;
    Ok(physical(parent)?.join(path.file_name().ok_or_else(|| error("invalid home path"))?))
}
fn private_parents(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::getuid().as_raw();
    for ancestor in path.ancestors() {
        match std::fs::metadata(ancestor) {
            Ok(info) => {
                if (info.uid() != 0 && info.uid() != uid)
                    || (info.mode() & 0o022 != 0 && info.mode() & 0o1000 == 0)
                {
                    return Err(error(
                        "unsafe account ancestor ownership or permissions; choose another home",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn private_home(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let info = std::fs::symlink_metadata(path)?;
    if !info.is_dir()
        || info.uid() != rustix::process::getuid().as_raw()
        || info.mode() & 0o022 != 0
    {
        return Err(error(
            "account home must be user-owned and not group/other-writable",
        ));
    }
    private_parents(path.parent().ok_or_else(|| error("invalid home parent"))?)
}
fn open_home(path: &Path, owner: Option<&Value>) -> Result<std::os::fd::OwnedFd> {
    private_home(path)?;
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| error("cannot open account directory"))?;
    let info = rustix::fs::fstat(&fd).map_err(|_| error("cannot inspect account directory"))?;
    let named = rustix::fs::lstat(path).map_err(|_| error("cannot verify account directory"))?;
    if named.st_dev != info.st_dev
        || named.st_ino != info.st_ino
        || owner.is_some_and(|r| {
            r["st_dev"].as_u64() != Some(info.st_dev as u64)
                || r["st_ino"].as_u64() != Some(info.st_ino as u64)
        })
    {
        return Err(error("account directory identity changed or unverified"));
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// Why `dir` cannot hold fixture roots: not absolute, inside the
    /// repository, or an ancestor from `/` that is not a real root- or
    /// user-owned directory without group/world write. `dir` is checked as
    /// given, so callers canonicalize first and a symlinked ancestor is refused.
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
                    m.file_type().is_dir()
                        && (m.uid() == 0 || m.uid() == uid)
                        && m.mode() & 0o022 == 0
                })
            })
            .map(|a| format!("unsafe ancestor {}", a.display()))
    }
    /// A root in the first candidate that canonicalizes, passes
    /// `parent_refusal` and takes a fresh `Fixture`; panics naming every
    /// refused candidate.
    fn select_parent(candidates: &[PathBuf], name: &str) -> Fixture {
        let mut refused = Vec::new();
        for candidate in candidates {
            let why = if !candidate.is_absolute() {
                "not absolute".to_string()
            } else {
                match candidate.canonicalize() {
                    Err(e) => format!("unresolvable: {e}"),
                    Ok(dir) => match parent_refusal(&dir) {
                        None => match Fixture::create(&dir, name) {
                            Ok(fixture) => return fixture,
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
    fn private_fixture(name: &str) -> Fixture {
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
        let fixture = select_parent(&candidates, name);
        let base = fixture.0.parent().unwrap();
        eprintln!("account fixture parent: {}", base.display());
        fixture
    }

    /// A fresh 0700 `oms-agy-<pid>-<name>-<hex>` root bound to its dev/ino:
    /// only that exact directory is removed, and never one that was replaced.
    struct Fixture(PathBuf, (u64, u64));
    impl Drop for Fixture {
        fn drop(&mut self) {
            let same = fs::symlink_metadata(&self.0)
                .is_ok_and(|m| m.file_type().is_dir() && (m.dev(), m.ino()) == self.1);
            if same && self.0.canonicalize().ok().as_ref() == Some(&self.0) {
                let _ = fs::remove_dir_all(&self.0);
            } else {
                eprintln!(
                    "fixture root {} was replaced; left in place",
                    self.0.display()
                );
            }
        }
    }
    impl Fixture {
        fn new(name: &str) -> Self {
            let fixture = private_fixture(name);
            fs::create_dir(fixture.config()).unwrap();
            mode(&fixture.config(), 0o700);
            fixture
        }
        /// `Err` only when the mkdir itself fails because `parent` cannot
        /// take a new entry (nothing was made, so another parent may be
        /// tried); any other mkdir error, or a made directory failing the
        /// checks, panics. A failed directory is removed only while it is
        /// still the empty one just made.
        fn create(parent: &Path, name: &str) -> std::io::Result<Self> {
            use std::io::ErrorKind::*;
            use std::io::Read;
            use std::os::unix::fs::DirBuilderExt;
            let mut bytes = [0u8; 8];
            fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut bytes))
                .unwrap();
            let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let root = parent.join(format!("oms-agy-{}-{name}-{suffix}", std::process::id()));
            // Non-recursive: refuses an existing path instead of adopting it.
            if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&root) {
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
                panic!("creating fixture root {}: {e}", root.display());
            }
            let m = fs::symlink_metadata(&root).unwrap();
            let id = (m.dev(), m.ino());
            if !(m.file_type().is_dir()
                && m.uid() == rustix::process::getuid().as_raw()
                && m.mode() & 0o777 == 0o700
                && root.canonicalize().ok().as_ref() == Some(&root))
            {
                if m.file_type().is_dir()
                    && fs::symlink_metadata(&root).is_ok_and(|n| (n.dev(), n.ino()) == id)
                {
                    let _ = fs::remove_dir(&root);
                }
                panic!(
                    "fixture root {} is not a fresh private directory",
                    root.display()
                );
            }
            Ok(Self(root, id))
        }
        fn config(&self) -> PathBuf {
            self.0.join("config")
        }
        /// A home with an obviously fake ADC placeholder (never a credential).
        fn home(&self, name: &str, profile: bool) -> PathBuf {
            let home = self.0.join(name);
            fs::create_dir(&home).unwrap();
            mode(&home, 0o700);
            if profile {
                put(&home.join(AGY_PROFILE), b"{\"placeholder\":true}\n");
            }
            home
        }
        fn run(&self, spec: &[&str], between: impl FnOnce()) -> Result<Output> {
            self.run_with_config(&self.config(), spec, between, || ())
        }
        fn run_with_hooks(
            &self,
            spec: &[&str],
            before_publish: impl FnOnce(),
            between_writes: impl FnOnce(),
        ) -> Result<Output> {
            self.run_with_config(&self.config(), spec, before_publish, between_writes)
        }
        fn run_with_config(
            &self,
            config: &Path,
            spec: &[&str],
            before_publish: impl FnOnce(),
            between_writes: impl FnOnce(),
        ) -> Result<Output> {
            let raw: Vec<OsString> = spec.iter().map(OsString::from).collect();
            let args = Args::parse(
                &raw,
                &["--account", "--home"],
                &["--first", "--last", "--remove", "--verify"],
            )
            .unwrap();
            let id = spec[1].split_once(':').unwrap().1;
            let claude = self.0.join(".claude");
            let host = AgyHost {
                config,
                default_claude: &claude,
                placement: &|_| Ok(()),
                mutate: &real_mutate,
                sync: &real_sync,
            };
            agy_locked(&args, id, &host, before_publish, between_writes)
        }
        /// Reports `phase` for every write to `file`, applying it first when
        /// `apply`; other files use the real write.
        fn run_fault(
            &self,
            spec: &[&str],
            file: &str,
            phase: oms_safe_fs::MutationPhase,
            apply: bool,
        ) -> Result<Output> {
            let raw: Vec<OsString> = spec.iter().map(OsString::from).collect();
            let args = Args::parse(
                &raw,
                &["--account", "--home"],
                &["--first", "--last", "--remove", "--verify"],
            )
            .unwrap();
            let id = spec[1].split_once(':').unwrap().1;
            let claude = self.0.join(".claude");
            let mutate =
                |root: &oms_safe_fs::Root, path: &Path, bytes: &[u8], old: Option<&[u8]>| {
                    if path.file_name().unwrap() != file {
                        return real_mutate(root, path, bytes, old);
                    }
                    if apply {
                        real_mutate(root, path, bytes, old).unwrap();
                    }
                    Err(oms_safe_fs::MutationError::Storage {
                        phase,
                        error: oms_safe_fs::ByteError::Io(rustix::io::Errno::IO),
                    })
                };
            let config = self.config();
            let host = AgyHost {
                config: &config,
                default_claude: &claude,
                placement: &|_| Ok(()),
                mutate: &mutate,
                sync: &real_sync,
            };
            agy_locked(&args, id, &host, || (), || ())
        }
        /// Real writes with the parent sync observed (and optionally failed).
        fn run_synced(&self, spec: &[&str], sync: &AgySync<'_>) -> Result<Output> {
            let raw: Vec<OsString> = spec.iter().map(OsString::from).collect();
            let args = Args::parse(
                &raw,
                &["--account", "--home"],
                &["--first", "--last", "--remove", "--verify"],
            )
            .unwrap();
            let id = spec[1].split_once(':').unwrap().1;
            let claude = self.0.join(".claude");
            let config = self.config();
            let host = AgyHost {
                config: &config,
                default_claude: &claude,
                placement: &|_| Ok(()),
                mutate: &real_mutate,
                sync,
            };
            agy_locked(&args, id, &host, || (), || ())
        }
        fn register(&self, id: &str, home: &Path, between: impl FnOnce()) -> Result<Output> {
            let account = format!("antigravity:{id}");
            self.run(
                &["--account", &account, "--home", home.to_str().unwrap()],
                between,
            )
        }
        /// Every config file and its bytes, to prove a refusal wrote nothing.
        fn files(&self) -> Vec<(OsString, Vec<u8>)> {
            let mut out: Vec<_> = fs::read_dir(self.config())
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    (e.file_name(), fs::read(e.path()).unwrap())
                })
                .collect();
            out.sort();
            out
        }
    }
    fn mode(path: &Path, bits: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
    }
    fn put(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        mode(path, 0o600);
    }
    fn refused<T>(result: Result<T>, needle: &str) {
        match result {
            Ok(_) => panic!("accepted; expected a refusal containing {needle:?}"),
            Err(e) => assert!(e.message.contains(needle), "{needle:?} not in {e:?}"),
        }
    }

    #[test]
    fn fixture_parent_selection_refuses_shared_symlinked_and_repo_paths() {
        let fx = Fixture::new("select");
        let at = |name: &str, bits: u32| {
            let dir = fx.0.join(name);
            fs::create_dir(&dir).unwrap();
            mode(&dir, bits);
            dir
        };
        let (good, other) = (at("good", 0o700), at("other", 0o755));
        let sticky = at("sticky", 0o1777);
        // Ancestry-safe but unwritable: selection must fall through, not panic.
        let locked = at("locked", 0o555);
        let locked_id = |dir: &Path| {
            let m = fs::symlink_metadata(dir).unwrap();
            let entries = fs::read_dir(dir).unwrap().count();
            (m.mode(), m.ino(), entries)
        };
        let locked_before = locked_id(&locked);
        assert_eq!(locked_before.2, 0);
        at("group", 0o770);
        let under_group = at("group/inner", 0o700);
        let link = fx.0.join("link");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let parent_of = |candidates: &[PathBuf]| {
            let fixture = select_parent(candidates, "pick");
            fixture.0.parent().unwrap().to_path_buf()
        };
        // Root bypasses the 0555 mode, so only a non-root run can see EACCES.
        let as_root = rustix::process::getuid().is_root();
        let picked = parent_of(&[sticky.clone(), locked.clone(), good.clone(), other.clone()]);
        if !as_root {
            assert_eq!(picked, good);
        }
        assert_eq!(locked_id(&locked), locked_before);
        // A symlinked candidate is judged by its physical target ...
        assert_eq!(parent_of(std::slice::from_ref(&link)), good);
        // ... and a symlink inside a path already taken as physical is refused.
        assert_eq!(
            parent_refusal(&link.join("inner")),
            Some(format!("unsafe ancestor {}", link.display()))
        );
        assert_eq!(parent_refusal(&other), None);
        let missing = fx.0.join("missing");
        let mut candidates = vec![
            sticky.clone(),
            under_group.clone(),
            repo.clone(),
            missing.clone(),
            PathBuf::from("relative"),
        ];
        let mut wants = vec![
            format!("{}: unsafe ancestor {}", sticky.display(), sticky.display()),
            format!(
                "{}: unsafe ancestor {}",
                under_group.display(),
                fx.0.join("group").display()
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
        assert_eq!(locked_id(&locked), locked_before);
    }

    #[test]
    fn publication_refuses_home_or_profile_changed_after_validation() {
        let fx = Fixture::new("race");
        fx.register("a", &fx.home("a", true), || ()).unwrap();
        let before = fx.files();
        type Swap = Box<dyn FnOnce(&Path)>;
        let cases: Vec<(&str, bool, Swap, &str)> = vec![
            (
                "renamed",
                true,
                Box::new(|home| {
                    let other = home.with_extension("other");
                    fs::create_dir(&other).unwrap();
                    mode(&other, 0o700);
                    fs::rename(home, home.with_extension("moved")).unwrap();
                    fs::rename(&other, home).unwrap();
                }),
                "home changed during registration",
            ),
            (
                "chmod-home",
                true,
                Box::new(|home| mode(home, 0o750)),
                "home changed during registration",
            ),
            (
                "replaced",
                true,
                Box::new(|home| {
                    put(&home.join("next"), b"{}\n");
                    fs::rename(home.join("next"), home.join(AGY_PROFILE)).unwrap();
                }),
                "ADC profile changed during registration",
            ),
            (
                "hardlinked",
                true,
                Box::new(|home| fs::hard_link(home.join(AGY_PROFILE), home.join("x")).unwrap()),
                "ADC profile changed during registration",
            ),
            (
                "chmod-profile",
                true,
                Box::new(|home| mode(&home.join(AGY_PROFILE), 0o644)),
                "ADC profile changed during registration",
            ),
            (
                "appeared",
                false,
                Box::new(|home| put(&home.join(AGY_PROFILE), b"{}\n")),
                "ADC profile changed during registration",
            ),
        ];
        for (name, profile, swap, needle) in cases {
            let home = fx.home(name, profile);
            refused(fx.register("b", &home, || swap(&home)), needle);
            assert_eq!(fx.files(), before, "{name}");
        }
        // The registry root is held too: a config directory swapped in after
        // validation is never written through.
        let config = fx.config();
        let home = fx.home("held-config", true);
        refused(
            fx.register("b", &home, || {
                fs::rename(&config, config.with_extension("moved")).unwrap();
                fs::create_dir(&config).unwrap();
                mode(&config, 0o700);
            }),
            "atomic write refused",
        );
        assert!(fx.files().is_empty());
        fs::remove_dir(&config).unwrap();
        fs::rename(config.with_extension("moved"), &config).unwrap();
        assert_eq!(fx.files(), before);
        fx.register("b", &home, || ()).unwrap();
    }

    #[test]
    fn empty_metadata_is_corrupt_never_missing() {
        let fx = Fixture::new("empty");
        let home = fx.home("a", true);
        for file in [AGY_REGISTRY, AGY_RECEIPT] {
            put(&fx.config().join(file), b"");
            let before = fx.files();
            for spec in [
                vec![
                    "--account",
                    "antigravity:a",
                    "--home",
                    home.to_str().unwrap(),
                ],
                vec!["--account", "antigravity:a", "--remove"],
                vec!["--account", "antigravity:a", "--verify"],
            ] {
                refused(
                    fx.run(&spec, || ()),
                    &format!("antigravity account metadata is empty (corrupt): {file}"),
                );
                assert_eq!(fx.files(), before);
            }
            refused(agy_homes(&fx.config()), "account metadata is empty");
            fs::remove_file(fx.config().join(file)).unwrap();
        }
        for file in [
            "provider-accounts.json",
            "provider-accounts.claude.json",
            "codex-account-links.json",
            "pi-account-links.json",
            "provider-account-effects.json",
        ] {
            put(&fx.config().join(file), b"");
            let before = fx.files();
            refused(
                fx.register("a", &home, || ()),
                &format!("provider account metadata is empty (corrupt): {file}"),
            );
            assert_eq!(fx.files(), before);
            fs::remove_file(fx.config().join(file)).unwrap();
        }
        fx.register("a", &home, || ()).unwrap();
    }

    #[test]
    fn malformed_foreign_rows_and_missing_receipts_refuse() {
        let fx = Fixture::new("foreign");
        let home = fx.home("a", true);
        let work = fx.home("work", false);
        let config = fx.config();
        let shared = json!({"schema":1,"providers":{"codex":{"accounts":[
            {"id":"default","home":fx.0.join("main")},{"id":"work","home":work}]}}});
        put(
            &config.join("provider-accounts.json"),
            &json_bytes(&shared).unwrap(),
        );
        let links = config.join("codex-account-links.json");
        let receipt = json!({"id":"work","home":work,"links":{},"st_dev":1,"st_ino":2});
        let missing = "missing codex account receipt for work (codex-account-links.json)";
        refused(fx.register("a", &home, || ()), missing);
        put(
            &links,
            &json_bytes(&json!([{"id":"other","home":work}])).unwrap(),
        );
        refused(
            fx.register("a", &home, || ()),
            "invalid provider account metadata",
        );
        put(&links, b"[]");
        refused(fx.register("a", &home, || ()), missing);
        let invalid = "invalid provider account metadata (codex-account-links.json)";
        for bad in [
            json!({"id":"work"}),
            json!([1]),
            json!([{"id":"work","home":work,"links":{},"extra":1}]),
            json!([{"id":"work","home":"relative","links":{}}]),
            json!([{"id":"Bad id","home":work,"links":{}}]),
            json!([{"id":"work","home":7,"links":{}}]),
            json!([{"id":"work","home":work,"links":{"unknown":"x"}}]),
            json!([{"id":"work","home":work,"links":{},"st_ino":"2"}]),
        ] {
            put(&links, &json_bytes(&bad).unwrap());
            let before = fx.files();
            refused(fx.register("a", &home, || ()), invalid);
            assert_eq!(fx.files(), before, "{bad}");
        }
        put(&links, b"{bad");
        refused(fx.register("a", &home, || ()), invalid);
        put(&links, &json_bytes(&json!([receipt])).unwrap());
        for (file, bad, needle) in [
            (
                "provider-accounts.claude.json",
                json!({"schema":1,"provider":"claude","accounts":[{"id":"Bad","home":work}]}),
                "invalid provider account metadata (provider-accounts.claude.json)",
            ),
            (
                "pi-account-links.json",
                json!({"accounts":[]}),
                "invalid provider account metadata (pi-account-links.json)",
            ),
            (
                "pi-account-links.json",
                json!([{"id":"p","home":work,"links":{"config.toml":"x"}}]),
                "invalid provider account metadata (pi-account-links.json)",
            ),
            (
                "provider-account-effects.json",
                json!({"schema":1,"accounts":[{"id":"x","home":work,"links":{},"provider":"codex"}]}),
                "invalid provider account metadata (provider-account-effects.json)",
            ),
            (
                "provider-accounts.claude.json",
                json!({"schema":1,"provider":"claude","accounts":[{"id":"c","home":fx.0.join("c")}]}),
                "missing claude account receipt for c (provider-account-effects.json)",
            ),
        ] {
            put(&config.join(file), &json_bytes(&bad).unwrap());
            let before = fx.files();
            refused(fx.register("a", &home, || ()), needle);
            assert_eq!(fx.files(), before, "{file}");
            fs::remove_file(config.join(file)).unwrap();
        }
        // A well-formed foreign registry still reserves its homes.
        refused(
            fx.register("a", &work, || ()),
            "already registered by another provider",
        );
        fx.register("a", &home, || ()).unwrap();

        // Codex, Claude and Pi registration read Antigravity rows the same way.
        let registry = config.join(AGY_REGISTRY);
        let receipts = config.join(AGY_RECEIPT);
        assert_eq!(
            agy_homes(&config).unwrap(),
            vec![home.clone(), home.clone()]
        );
        let good = fs::read(&receipts).unwrap();
        put(&receipts, b"[]");
        refused(
            agy_homes(&config),
            "missing antigravity account receipt for a",
        );
        put(&receipts, &good);
        let mut doc: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        doc["accounts"][0]["extra"] = json!(1);
        put(&registry, &json_bytes(&doc).unwrap());
        refused(agy_homes(&config), "invalid provider account metadata");
        put(&registry, b"{bad");
        refused(agy_homes(&config), "invalid provider account metadata");
    }

    #[test]
    fn between_write_failure_and_aliased_config_root() {
        let fx = Fixture::new("partial");
        let a = fx.home("a", true);
        let b = fx.home("b", true);
        let a_profile = a.join(AGY_PROFILE);
        let a_bytes = fs::read(&a_profile).unwrap();
        let a_ino = fs::symlink_metadata(&a_profile).unwrap().ino();
        let reg_path = fx.config().join(AGY_REGISTRY);
        let rec_path = fx.config().join(AGY_RECEIPT);

        // 1. First registration: receipt succeeds, registry write is obstructed
        // by a directory -> honest partial publication error, ADC untouched,
        // no placeholder leakage, deterministic recovery once cleared.
        let a_spec = ["--account", "antigravity:a", "--home", a.to_str().unwrap()];
        let err = fx
            .run_with_hooks(
                &a_spec,
                || (),
                || {
                    fs::create_dir(&reg_path).unwrap();
                },
            )
            .err()
            .expect("obstructed second write must fail");
        assert!(
            err.message.contains("partial publication failure")
                && err.message.contains(AGY_RECEIPT)
                && err.message.contains(AGY_REGISTRY)
                && err.message.contains("ADC profile was not modified")
                && !err.message.contains("nothing was changed")
                && !err.message.contains("placeholder"),
            "{err:?}"
        );
        assert!(rec_path.is_file());
        assert!(reg_path.is_dir());
        assert_eq!(fs::read(&a_profile).unwrap(), a_bytes);
        assert_eq!(fs::symlink_metadata(&a_profile).unwrap().ino(), a_ino);
        fs::remove_dir(&reg_path).unwrap();
        fx.run(&a_spec, || ()).unwrap();
        assert_eq!(
            fx.run(&["--account", "antigravity:a", "--verify"], || ())
                .unwrap()
                .code,
            0
        );

        // 2. Subsequent registration: receipt updated, registry write fails ->
        // receipt rolls back to prior bytes, ADC untouched, rerun succeeds.
        let before_rec = fs::read(&rec_path).unwrap();
        let before_reg = fs::read(&reg_path).unwrap();
        let b_spec = ["--account", "antigravity:b", "--home", b.to_str().unwrap()];
        let b_profile = b.join(AGY_PROFILE);
        let b_bytes = fs::read(&b_profile).unwrap();
        let err = fx
            .run_with_hooks(
                &b_spec,
                || (),
                || {
                    fs::remove_file(&reg_path).unwrap();
                    fs::create_dir(&reg_path).unwrap();
                },
            )
            .err()
            .expect("obstructed registry update must fail");
        assert!(
            err.message.contains("rolled back")
                && err.message.contains(AGY_RECEIPT)
                && err.message.contains("ADC profile was not modified"),
            "{err:?}"
        );
        assert_eq!(fs::read(&rec_path).unwrap(), before_rec);
        assert_eq!(fs::read(&b_profile).unwrap(), b_bytes);
        fs::remove_dir(&reg_path).unwrap();
        put(&reg_path, &before_reg);
        fx.run(&b_spec, || ()).unwrap();

        // 3. Removal between-write failure: registry write succeeds, receipt
        // write fails -> registry rolls back, home & ADC untouched, rerun succeeds.
        let before_reg = fs::read(&reg_path).unwrap();
        let before_rec = fs::read(&rec_path).unwrap();
        let err = fx
            .run_with_hooks(
                &["--account", "antigravity:b", "--remove"],
                || (),
                || {
                    fs::remove_file(&rec_path).unwrap();
                    fs::create_dir(&rec_path).unwrap();
                },
            )
            .err()
            .expect("obstructed receipt removal must fail");
        assert!(
            err.message.contains("rolled back")
                && err.message.contains(AGY_REGISTRY)
                && err
                    .message
                    .contains("home and ADC profile were not modified"),
            "{err:?}"
        );
        assert_eq!(fs::read(&reg_path).unwrap(), before_reg);
        assert_eq!(fs::read(&b_profile).unwrap(), b_bytes);
        fs::remove_dir(&rec_path).unwrap();
        put(&rec_path, &before_rec);
        fx.run(&["--account", "antigravity:b", "--remove"], || ())
            .unwrap();

        // 4. Aliased config root spelling resolves to the same physical root,
        // while an unsafe symlinked config target is refused cleanly.
        let alias_cfg = fx.0.join("cfg-alias");
        std::os::unix::fs::symlink(fx.config(), &alias_cfg).unwrap();
        fx.run_with_config(&alias_cfg, &b_spec, || (), || ())
            .unwrap();
        let reg_doc: Value = serde_json::from_slice(&fs::read(&reg_path).unwrap()).unwrap();
        assert_eq!(reg_doc["accounts"].as_array().unwrap().len(), 2);

        let unsafe_dir = fx.0.join("unsafe-cfg");
        fs::create_dir(&unsafe_dir).unwrap();
        mode(&unsafe_dir, 0o777);
        let unsafe_alias = fx.0.join("unsafe-alias");
        std::os::unix::fs::symlink(&unsafe_dir, &unsafe_alias).unwrap();
        refused(
            fx.run_with_config(&unsafe_alias, &b_spec, || (), || ()),
            "unsafe antigravity account registry ancestry",
        );
    }

    #[test]
    fn published_and_uncertain_writes_reconcile_without_rollback() {
        use oms_safe_fs::MutationPhase::{PublicationUncertain, Published};
        let fx = Fixture::new("outcomes");
        let a = fx.home("a", true);
        let a_profile = a.join(AGY_PROFILE);
        let a_bytes = fs::read(&a_profile).unwrap();
        let reg = fx.config().join(AGY_REGISTRY);
        let rec = fx.config().join(AGY_RECEIPT);
        let spec = ["--account", "antigravity:a", "--home", a.to_str().unwrap()];
        let remove = ["--account", "antigravity:a", "--remove"];
        let msg = |r: Result<Output>| r.err().expect("fault must fail").message;
        let count = |path: &Path| -> usize {
            let doc: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            doc.get("accounts")
                .unwrap_or(&doc)
                .as_array()
                .unwrap()
                .len()
        };
        let cases = [
            (Published, true),
            (PublicationUncertain, true),
            (PublicationUncertain, false),
        ];

        // Second write published/uncertain: the first file is not rolled back
        // and the same command reconciles, idempotently.
        for (phase, apply) in cases {
            let m = msg(fx.run_fault(&spec, AGY_REGISTRY, phase, apply));
            assert!(
                m.contains("unconfirmed")
                    && m.contains("was not rolled back")
                    && m.contains("ADC profile was not modified"),
                "{m}"
            );
            assert_eq!(count(&rec), 1);
            assert_eq!(
                if reg.exists() { count(&reg) } else { 0 },
                usize::from(apply)
            );
            fx.run(&spec, || ()).unwrap();
            assert_eq!(
                fx.run(&["--account", "antigravity:a", "--verify"], || ())
                    .unwrap()
                    .code,
                0
            );
            let snapshot = fx.files();
            fx.run(&spec, || ()).unwrap();
            assert_eq!(fx.files(), snapshot);

            let m = msg(fx.run_fault(&remove, AGY_RECEIPT, phase, apply));
            assert!(
                m.contains("unconfirmed")
                    && m.contains("was not rolled back")
                    && m.contains("rerun the same --remove"),
                "{m}"
            );
            assert_eq!(count(&reg), 0);
            fx.run(&remove, || ()).unwrap();
            assert_eq!(count(&rec), 0);
        }

        // First write faulty: the sibling is never written; retry completes.
        for (phase, apply) in [(Published, true), (PublicationUncertain, false)] {
            let m = msg(fx.run_fault(&spec, AGY_RECEIPT, phase, apply));
            assert!(
                m.contains("unconfirmed") && m.contains("was not written"),
                "{m}"
            );
            assert_eq!(count(&reg), 0);
            fx.run(&spec, || ()).unwrap();
            let m = msg(fx.run_fault(&remove, AGY_REGISTRY, phase, apply));
            assert!(
                m.contains("unconfirmed") && m.contains("was not written"),
                "{m}"
            );
            assert_eq!(count(&rec), 1);
            fx.run(&remove, || ()).unwrap();
        }
        assert_eq!(fs::read(&a_profile).unwrap(), a_bytes);
    }

    #[test]
    fn already_removed_retry_syncs_the_parent_before_settling() {
        use oms_safe_fs::{ByteError, MutationPhase::Published};
        use std::cell::Cell;
        let fx = Fixture::new("settle");
        let (a, b) = (fx.home("a", true), fx.home("b", true));
        let spec = |id: &str, home: &Path| {
            vec![
                "--account".to_owned(),
                format!("antigravity:{id}"),
                "--home".to_owned(),
                home.to_str().unwrap().to_owned(),
            ]
        };
        let run = |args: &[String], sync: &AgySync<'_>| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            fx.run_synced(&args, sync)
        };
        let calls = Cell::new(0);
        let counting = |root: &oms_safe_fs::Root, path: &Path| {
            calls.set(calls.get() + 1);
            real_sync(root, path)
        };
        let failing = |_: &oms_safe_fs::Root, _: &Path| Err(ByteError::Io(rustix::io::Errno::IO));

        // A fresh missing-state removal has no metadata to prove a parent.
        let remove = ["--account", "antigravity:a", "--remove"];
        let message =
            String::from_utf8(run(&remove.map(str::to_owned), &counting).unwrap().out).unwrap();
        assert!(message.contains("nothing changed"), "{message}");
        assert_eq!(calls.get(), 0);
        assert!(!fx.config().join(AGY_REGISTRY).exists());

        fx.register("a", &a, || ()).unwrap();
        fx.register("b", &b, || ()).unwrap();
        // The second write is applied but reported unconfirmed: the target is
        // already absent from both files, so the retry sees nothing to remove.
        let m = fx
            .run_fault(&remove, AGY_RECEIPT, Published, true)
            .err()
            .unwrap()
            .message;
        assert!(m.contains("unconfirmed"), "{m}");
        let snapshot = fx.files();

        refused(
            run(&remove.map(str::to_owned), &failing),
            "durability unconfirmed",
        );
        assert_eq!(fx.files(), snapshot);
        calls.set(0);
        let message =
            String::from_utf8(run(&remove.map(str::to_owned), &counting).unwrap().out).unwrap();
        assert!(message.contains("nothing changed"), "{message}");
        assert_eq!(calls.get(), 1);
        assert_eq!(fx.files(), snapshot);
        // The unrelated account survives in both files.
        for file in [AGY_REGISTRY, AGY_RECEIPT] {
            assert!(
                fs::read_to_string(fx.config().join(file))
                    .unwrap()
                    .contains("\"b\"")
            );
            assert!(
                !fs::read_to_string(fx.config().join(file))
                    .unwrap()
                    .contains("\"a\"")
            );
        }

        // An unchanged registration retry also confirms the parent, and an
        // unconfirmed sync is an error, not success.
        let again = spec("b", &b);
        calls.set(0);
        run(&again, &counting).unwrap();
        assert_eq!(calls.get(), 2);
        refused(run(&again, &failing), "durability unconfirmed");
        assert_eq!(fx.files(), snapshot);
    }

    #[test]
    fn login_spec_and_menu_parsing() {
        assert_eq!(
            parse_login_spec("codex").unwrap(),
            LoginTarget {
                provider: "codex".into(),
                account_id: None,
            }
        );
        assert_eq!(
            parse_login_spec("codex:work").unwrap(),
            LoginTarget {
                provider: "codex".into(),
                account_id: Some("work".into()),
            }
        );
        assert_eq!(
            parse_login_spec("claude:work").unwrap(),
            LoginTarget {
                provider: "claude".into(),
                account_id: Some("work".into()),
            }
        );
        assert_eq!(
            parse_login_spec("antigravity").unwrap(),
            LoginTarget {
                provider: "antigravity".into(),
                account_id: None,
            }
        );
        for bad in ["antigravity:work", "antigravity:default", "antigravity:"] {
            refused(
                parse_login_spec(bad),
                "antigravity does not support per-account sign-in",
            );
        }
        for bad in ["codex:", "codex:Bad", "claude:UPPER"] {
            refused(parse_login_spec(bad), "invalid provider account id");
        }
        for bad in ["", "agy", "pi", "gemini:work"] {
            refused(parse_login_spec(bad), "unsupported login provider");
        }

        assert_eq!(parse_menu_choice("1\n", 4).unwrap(), Some(0));
        assert_eq!(parse_menu_choice(" 4 ", 4).unwrap(), Some(3));
        assert_eq!(parse_menu_choice("q\n", 4).unwrap(), None);
        assert_eq!(parse_menu_choice("Q", 4).unwrap(), None);
        assert_eq!(parse_menu_choice("  \n", 4).unwrap(), None);
        for bad in ["0", "5", "-1", "abc", "1 2"] {
            refused(parse_menu_choice(bad, 4), "invalid selection");
        }

        let menu = format_login_menu(&[
            MenuItem {
                entry: LoginEntry {
                    provider: "codex".into(),
                    account_id: None,
                    home_env: None,
                },
                status: LoginStatus::SignedIn.label(),
            },
            MenuItem {
                entry: LoginEntry {
                    provider: "codex".into(),
                    account_id: Some("work".into()),
                    home_env: Some(("CODEX_HOME", PathBuf::from("codex-work"))),
                },
                status: LoginStatus::NotSignedIn.label(),
            },
            MenuItem {
                entry: LoginEntry {
                    provider: "claude".into(),
                    account_id: Some("bad".into()),
                    home_env: Some(("CLAUDE_CONFIG_DIR", PathBuf::from("claude-bad"))),
                },
                status: LoginStatus::Unknown("unsafe or missing home".into()).label(),
            },
            MenuItem {
                entry: LoginEntry {
                    provider: "antigravity".into(),
                    account_id: None,
                    home_env: None,
                },
                status: "sign-in not verifiable".into(),
            },
        ]);
        assert!(menu.contains("1. codex (default)"), "{menu}");
        assert!(menu.contains("[signed in]"), "{menu}");
        assert!(menu.contains("2. codex:work"), "{menu}");
        assert!(menu.contains("[not signed in]"), "{menu}");
        assert!(menu.contains("3. claude:bad"), "{menu}");
        assert!(menu.contains("[unknown: unsafe or missing home]"), "{menu}");
        assert!(menu.contains("4. antigravity"), "{menu}");
        assert!(menu.contains("[sign-in not verifiable]"), "{menu}");
    }

    #[test]
    fn login_status_reports_three_states_and_menu_flags_unsafe_home() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        let sh = |script: &str| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]).stdin(Stdio::null());
            cmd
        };
        let clear = AtomicBool::new(false);
        let interrupted = AtomicBool::new(true);

        let ok_status =
            run_login_status_command(sh("exit 0"), "codex", &clear, LOGIN_STATUS_TIMEOUT).unwrap();
        assert_eq!(ok_status, LoginStatus::SignedIn);
        assert_eq!(ok_status.exit_code(), 0);
        assert_eq!(ok_status.label(), "signed in");

        let no_status =
            run_login_status_command(sh("exit 1"), "codex", &clear, LOGIN_STATUS_TIMEOUT).unwrap();
        assert_eq!(no_status, LoginStatus::NotSignedIn);
        assert_eq!(no_status.exit_code(), 1);
        assert_eq!(no_status.label(), "not signed in");

        // Sleeping past a short timeout cap reports unknown (never "not signed in").
        let timed_out =
            run_login_status_command(sh("sleep 5"), "codex", &clear, Duration::from_millis(100))
                .unwrap();
        assert!(
            matches!(&timed_out, LoginStatus::Unknown(reason) if reason.contains("timed out")),
            "{timed_out:?}"
        );
        assert_eq!(timed_out.exit_code(), LOGIN_UNKNOWN_CODE);
        assert_ne!(timed_out.exit_code(), 0);
        assert_ne!(timed_out.exit_code(), 1);

        // Explicit 124 / 130 exit codes and pre-set interrupted flag all report unknown.
        let code_124 =
            run_login_status_command(sh("exit 124"), "codex", &clear, LOGIN_STATUS_TIMEOUT)
                .unwrap();
        assert!(
            matches!(&code_124, LoginStatus::Unknown(reason) if reason.contains("timed out")),
            "{code_124:?}"
        );
        assert_eq!(code_124.exit_code(), LOGIN_UNKNOWN_CODE);

        let code_130 =
            run_login_status_command(sh("exit 130"), "claude", &clear, LOGIN_STATUS_TIMEOUT)
                .unwrap();
        assert!(
            matches!(&code_130, LoginStatus::Unknown(reason) if reason.contains("interrupted")),
            "{code_130:?}"
        );
        assert_eq!(code_130.exit_code(), LOGIN_UNKNOWN_CODE);

        let pre_interrupted =
            run_login_status_command(sh("exit 0"), "claude", &interrupted, LOGIN_STATUS_TIMEOUT)
                .unwrap();
        assert!(
            matches!(&pre_interrupted, LoginStatus::Unknown(reason) if reason.contains("interrupted")),
            "{pre_interrupted:?}"
        );
        assert_eq!(pre_interrupted.exit_code(), LOGIN_UNKNOWN_CODE);

        // Menu rows: default home passes None (no env override), unsafe or missing registered
        // homes report "unknown: unsafe or missing home" without probing status, and unknown
        // status results surface directly in the menu.
        let fx = Fixture::new("login-menu");
        let good_home = fx.home("good", false);
        let unsafe_home = fx.home("unsafe", false);
        mode(&unsafe_home, 0o777);
        let missing_home = fx.0.join("missing");

        let mut probed: Vec<(String, Option<PathBuf>)> = Vec::new();
        let items = build_login_menu_with(
            |provider| match provider {
                "codex" => Ok(vec![
                    ("default".into(), fx.0.join("unused-default")),
                    ("work".into(), good_home.clone()),
                    ("bad".into(), unsafe_home.clone()),
                ]),
                "claude" => Ok(vec![
                    ("default".into(), fx.0.join("unused-claude-default")),
                    ("gone".into(), missing_home.clone()),
                ]),
                _ => Ok(Vec::new()),
            },
            |_| true,
            |provider, home| {
                probed.push((provider.to_owned(), home.map(Path::to_path_buf)));
                match (provider, home) {
                    ("codex", None) => Ok(LoginStatus::SignedIn),
                    ("codex", Some(_)) => Ok(LoginStatus::Unknown("status check timed out".into())),
                    ("claude", None) => Ok(LoginStatus::NotSignedIn),
                    _ => panic!("unexpected status probe for {provider} {home:?}"),
                }
            },
        )
        .unwrap();

        assert_eq!(items.len(), 6);
        assert_eq!(items[0].entry.account_id, None);
        assert_eq!(items[0].entry.home_env, None);
        assert_eq!(items[0].status, "signed in");
        assert_eq!(items[1].entry.account_id.as_deref(), Some("work"));
        assert_eq!(
            items[1].entry.home_env,
            Some(("CODEX_HOME", good_home.clone()))
        );
        assert_eq!(items[1].status, "unknown: status check timed out");
        assert_eq!(items[2].entry.account_id.as_deref(), Some("bad"));
        assert_eq!(items[2].status, "unknown: unsafe or missing home");
        assert_eq!(items[3].entry.account_id, None);
        assert_eq!(items[3].entry.home_env, None);
        assert_eq!(items[3].status, "not signed in");
        assert_eq!(items[4].entry.account_id.as_deref(), Some("gone"));
        assert_eq!(items[4].status, "unknown: unsafe or missing home");
        assert_eq!(items[5].entry.provider, "antigravity");
        assert_eq!(items[5].status, "sign-in not verifiable");

        assert_eq!(
            probed,
            vec![
                ("codex".into(), None),
                ("codex".into(), Some(good_home)),
                ("claude".into(), None),
            ]
        );
    }
}
