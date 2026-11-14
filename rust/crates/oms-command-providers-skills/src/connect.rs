use crate::common::*;
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
fn account(args: &Args, ctx: &Context<'_>) -> Result<Output> {
    let (provider, id) = args
        .required("--account")?
        .split_once(':')
        .ok_or_else(|| error("--account requires PROVIDER:ID"))?;
    if !["codex", "claude"].contains(&provider) {
        return Err(error(
            "provider account isolation is unproved and unavailable",
        ));
    }
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        || (id == "default" && !args.has("--verify"))
    {
        return Err(error("invalid provider account id; default is reserved"));
    }
    if provider == "claude" {
        return Err(error(
            "native Claude account hook installer is unavailable; existing homes, registry and effects remain unchanged",
        ));
    }
    if args.has("--first") && args.has("--last") {
        return Err(error("choose --first or --last"));
    }
    let config = env_path("XDG_CONFIG_HOME", home()?.join(".config")).join("oh-my-setting");
    let config = physical(&config)?;
    let registry = config.join("provider-accounts.json");
    let receipt = config.join("codex-account-links.json");
    let _guard = lock(&registry)?;
    let old = optional(&registry)?;
    let mut value = if old.is_empty() {
        json!({"schema":1,"providers":{}})
    } else {
        json(&old)?
    };
    if value["schema"] != 1 || !value["providers"].is_object() {
        return Err(error("unsupported provider accounts schema"));
    }
    let rows_raw = optional(&receipt)?;
    let mut rows = if rows_raw.is_empty() {
        json!([])
    } else {
        json(&rows_raw)?
    };
    let rows = rows
        .as_array_mut()
        .ok_or_else(|| error("invalid Codex link ownership receipt"))?;
    let main = env_path("CODEX_HOME", home()?.join(".codex"));
    let main = physical(&main)?;
    let existing = value["providers"]["codex"]["accounts"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["id"] == id))
        .cloned();
    if args.has("--verify") {
        let all = value["providers"]["codex"]["accounts"]
            .as_array()
            .ok_or_else(|| error("Codex account is not registered"))?;
        let mut text = String::new();
        let mut failed = false;
        for row in std::iter::once(json!({"id":"main","home":main})).chain(all.iter().cloned()) {
            let path = PathBuf::from(
                row["home"]
                    .as_str()
                    .ok_or_else(|| error("invalid account home"))?,
            );
            let good = private_home(&path).is_ok();
            let mut command = Command::new("codex");
            command
                .args(["login", "status"])
                .env("CODEX_HOME", path)
                .stdin(Stdio::null());
            let verified = good && capture(command, ctx.interrupted, 15)?.code == 0;
            failed |= !verified;
            text.push_str(&format!(
                "{}: codex account {} {}\n",
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
        let mut kept = vec![];
        for index in 0..rows.len() {
            if rows[index]["id"] != id {
                continue;
            }
            let row = &rows[index];
            let path = PathBuf::from(
                row["home"]
                    .as_str()
                    .ok_or_else(|| error("invalid receipt home"))?,
            );
            if !path.exists() {
                continue;
            }
            let directory = open_home(&path, Some(row))?;
            let links = row["links"]
                .as_object()
                .ok_or_else(|| error("invalid receipt links"))?
                .clone();
            for (name, target) in links {
                if !SHARED.contains(&name.as_str()) {
                    return Err(error("invalid recorded account link"));
                }
                let expected = target
                    .as_str()
                    .ok_or_else(|| error("invalid recorded link target"))?;
                match rustix::fs::readlinkat(&directory, &name, Vec::new()) {
                    Ok(actual) if actual.as_bytes() == expected.as_bytes() => {
                        rustix::fs::unlinkat(&directory, &name, rustix::fs::AtFlags::empty())
                            .map_err(|_| error("safe account link removal failed"))?;
                    }
                    Err(rustix::io::Errno::NOENT) => (),
                    _ => {
                        kept.push(name.clone());
                        continue;
                    }
                }
                rows[index]["links"]
                    .as_object_mut()
                    .ok_or_else(|| error("invalid receipt links"))?
                    .remove(&name);
                write_locked(&receipt, &json_bytes(&json!(rows))?, false, None)?;
            }
        }
        if !kept.is_empty() {
            return Err(error(
                "changed account links kept; ownership retained for explicit recovery",
            ));
        }
        rows.retain(|r| r["id"] != id);
        write_locked(&receipt, &json_bytes(&json!(rows))?, false, None)?;
        if let Some(entries) = value["providers"]["codex"]["accounts"].as_array_mut() {
            entries.retain(|r| r["id"] != id);
        }
        if value["providers"]["codex"]["accounts"]
            .as_array()
            .is_some_and(Vec::is_empty)
        {
            value["providers"]
                .as_object_mut()
                .ok_or_else(|| error("invalid providers"))?
                .remove("codex");
        }
        write_locked(&registry, &json_bytes(&value)?, false, Some(&old))?;
        return Ok(Output::text(format!(
            "Codex account removed: {id}; home, login and sessions kept\n"
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
    for settings in value["providers"]
        .as_object()
        .ok_or_else(|| error("invalid providers"))?
        .values()
    {
        for row in settings["accounts"]
            .as_array()
            .ok_or_else(|| error("invalid provider accounts"))?
        {
            if row["id"] != id
                && row["home"]
                    .as_str()
                    .is_some_and(|s| physical(Path::new(s)).is_ok_and(|p| p == path))
            {
                return Err(error("duplicate provider account id or canonical home"));
            }
        }
    }
    let claude = optional(&config.join("provider-accounts.claude.json"))?;
    if !claude.is_empty() {
        for row in json(&claude)?["accounts"]
            .as_array()
            .ok_or_else(|| error("invalid Claude registry"))?
        {
            if row["home"].as_str().is_some_and(|s| Path::new(s) == path) {
                return Err(error(
                    "canonical home is already registered by another provider",
                ));
            }
        }
    }
    private_parents(
        path.parent()
            .ok_or_else(|| error("invalid account parent"))?,
    )?;
    if !path.exists() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)?;
    }
    private_home(&path)?;
    let owner = rows
        .iter()
        .find(|r| r["id"] == id && r["home"] == path.to_string_lossy().as_ref())
        .cloned();
    let directory = open_home(&path, owner.as_ref())?;
    let info =
        rustix::fs::fstat(&directory).map_err(|_| error("cannot inspect account directory"))?;
    if owner.is_none() {
        rows.push(
            json!({"id":id,"home":path,"links":{},"st_dev":info.st_dev,"st_ino":info.st_ino}),
        );
        write_locked(&receipt, &json_bytes(&json!(rows))?, false, None)?;
    }
    let index = rows
        .iter()
        .position(|r| r["id"] == id && r["home"] == path.to_string_lossy().as_ref())
        .ok_or_else(|| error("missing account ownership"))?;
    for name in SHARED {
        let target = main.join(name);
        let current = rustix::fs::statat(&directory, *name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW);
        if let Ok(info) = current {
            if rustix::fs::FileType::from_raw_mode(info.st_mode) != rustix::fs::FileType::Symlink {
                continue;
            }
            let current = rustix::fs::readlinkat(&directory, *name, Vec::new())
                .map_err(|_| error("cannot inspect account link"))?;
            if current.as_bytes() == target.as_os_str().as_encoded_bytes() {
                continue;
            }
            if !args.has("--repair") {
                continue;
            }
            if rows[index]["links"][*name].as_str() != std::str::from_utf8(current.as_bytes()).ok()
            {
                return Err(error("unowned conflicting account link kept"));
            }
            rustix::fs::unlinkat(&directory, *name, rustix::fs::AtFlags::empty())
                .map_err(|_| error("cannot safely remove account link"))?;
        }
        rows[index]["links"][*name] = json!(target);
        write_locked(&receipt, &json_bytes(&json!(rows))?, false, None)?;
        rustix::fs::symlinkat(&target, &directory, *name)
            .map_err(|_| error("cannot safely create account link"))?;
    }
    if value["providers"].get("codex").is_none() {
        value["providers"]["codex"] =
            json!({"policy":"ordered","accounts":[{"id":"default","home":main}]});
    }
    let entries = value["providers"]["codex"]["accounts"]
        .as_array_mut()
        .ok_or_else(|| error("invalid account list"))?;
    entries.retain(|r| r["id"] != id);
    let entry = json!({"id":id,"home":path});
    if args.has("--first") {
        entries.insert(0, entry);
    } else {
        entries.push(entry);
    }
    write_locked(&registry, &json_bytes(&value)?, false, Some(&old))?;
    Ok(Output::text(format!(
        "Codex account {id}: registered, login pending\n"
    )))
}
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
