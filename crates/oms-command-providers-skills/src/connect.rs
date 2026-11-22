use crate::common::*;
use oms_context::provider_accounts::{self as accounts, Dialect, valid_account_id};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn run(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        raw,
        &["--data-source-id", "--account", "--home"],
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
        ],
    )?;
    args.count(0)?;
    host_mutation()?;
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
fn validate_account_row(row: &Value, allow_default: bool) -> Result<(&str, &Path)> {
    let (id, home) = accounts::account_row(row).map_err(|_| error("invalid provider accounts"))?;
    if !allow_default && id == "default" {
        return Err(error("invalid provider accounts"));
    }
    Ok((id, home))
}
fn validate_shared_registry(value: &Value) -> Result<std::collections::BTreeSet<PathBuf>> {
    let providers = accounts::shared_providers(value, Dialect::Strict)
        .map_err(|_| error("unsupported provider accounts schema"))?;
    let mut all_homes = std::collections::BTreeSet::new();
    for (owner, settings) in providers {
        if accounts::home_descriptor(owner).is_none() {
            return Err(error("provider account isolation is unproved"));
        }
        if !accounts::settings_fields(settings)
            || accounts::policy(settings, Dialect::Strict).is_none()
        {
            return Err(error("invalid provider accounts"));
        }
        let Some(entries) = settings["accounts"].as_array().filter(|a| !a.is_empty()) else {
            return Err(error("invalid provider accounts"));
        };
        let mut ids = std::collections::BTreeSet::new();
        for row in entries {
            let (rid, rhome) = validate_account_row(row, true)?;
            let canon = physical(rhome)?;
            accounts::reserve_identity(&mut ids, &mut all_homes, rid, &canon)
                .map_err(|_| error("duplicate provider account id or canonical home"))?;
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
    let rows = accounts::claude_accounts(doc, Dialect::Strict)
        .map_err(|_| error("invalid Claude registry"))?;
    if accounts::policy(doc, Dialect::Strict).is_none() {
        return Err(error("invalid Claude registry"));
    }
    for row in rows {
        validate_account_row(row, false).map_err(|_| error("invalid Claude registry"))?;
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut homes = std::collections::BTreeSet::new();
    homes.insert(default_claude.to_owned());
    for row in rows {
        let rid = row["id"].as_str().unwrap_or_default();
        let rhome = physical(Path::new(row["home"].as_str().unwrap_or_default()))?;
        accounts::reserve_identity(&mut ids, &mut homes, rid, &rhome)
            .map_err(|_| error("duplicate provider account id or canonical home"))?;
        if shared["providers"].get("claude").is_none() && shared_homes.contains(&rhome) {
            return Err(error("duplicate provider account id or canonical home"));
        }
    }
    if !rows.is_empty()
        && shared["providers"].get("claude").is_none()
        && shared_homes.contains(default_claude)
    {
        return Err(error("duplicate provider account id or canonical home"));
    }
    Ok(())
}
fn account(args: &Args, ctx: &Context<'_>) -> Result<Output> {
    let (provider, id) = args
        .required("--account")?
        .split_once(':')
        .ok_or_else(|| error("--account requires PROVIDER:ID"))?;
    let spec = accounts::home_descriptor(provider)
        .ok_or_else(|| error("provider account isolation is unproved and unavailable"))?;
    let (env, default) = (spec.env, spec.home_sub);
    let (label, names, receipt_name): (&str, &[&str], &str) = match provider {
        "codex" => ("Codex", SHARED, "codex-account-links.json"),
        "claude" => ("Claude", CLAUDE_SHARED, "provider-account-effects.json"),
        "pi" => ("Pi", PI_SHARED, "pi-account-links.json"),
        _ => {
            return Err(error(
                "provider account isolation is unproved and unavailable",
            ));
        }
    };
    let claude = provider == "claude";
    if !valid_account_id(id) || (id == "default" && !args.has("--verify")) {
        return Err(error("invalid provider account id; default is reserved"));
    }
    if args.has("--first") && args.has("--last") {
        return Err(error("choose --first or --last"));
    }
    let config = env_path("XDG_CONFIG_HOME", home()?.join(".config")).join("oh-my-setting");
    let config = physical(&config)?;
    let registry = config.join("provider-accounts.json");
    let claude_registry = config.join("provider-accounts.claude.json");
    let receipt = config.join(receipt_name);
    let _guard = lock(&registry)?;
    let old = optional(&registry)?;
    let mut value = if old.is_empty() {
        json!({"schema":1,"providers":{}})
    } else {
        accounts::decode(&old, 4 * 1024 * 1024).map_err(|_| error("invalid JSON"))?
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
            let parsed = accounts::decode(&claude_raw, 4 * 1024 * 1024)
                .map_err(|_| error("invalid JSON"))?;
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
                    let _ = login_verified("claude", &path, ctx)?;
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
            let verified = good && login_verified(provider, &path, ctx)?;
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
    if path == main {
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
fn login_verified(provider: &str, path: &Path, ctx: &Context<'_>) -> Result<bool> {
    let (binary, args, env): (&str, &[&str], &str) = match provider {
        "codex" => ("codex", &["login", "status"], "CODEX_HOME"),
        "claude" => ("claude", &["auth", "status"], "CLAUDE_CONFIG_DIR"),
        _ => return Ok(false),
    };
    let mut command = Command::new(binary);
    command.args(args).env(env, path).stdin(Stdio::null());
    Ok(capture(command, ctx.interrupted, 15)?.code == 0)
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
