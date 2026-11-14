use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) fn expected(paths: &Paths) -> Result<Value> {
    let raw = String::from_utf8(r::read(
        &paths.source.join("scripts/install-claude-hooks.sh"),
        r::LIMIT,
    )?)
    .map_err(|_| r::Error(2, "invalid hook source".into()))?;
    let schema = raw
        .lines()
        .find_map(|s| s.strip_prefix("HOOKS_SCHEMA="))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| r::Error(2, "hook schema missing".into()))?;
    let surfaces = raw
        .split_once("HOOK_SURFACES='\n")
        .and_then(|(_, s)| s.split_once("\n'\n"))
        .map(|(s, _)| s)
        .ok_or_else(|| r::Error(2, "hook surfaces missing".into()))?;
    let mut rows = Vec::new();
    for line in surfaces
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let parts = line.split('|').collect::<Vec<_>>();
        if parts.len() < 4 || !r::safe_name(parts[1]) {
            return r::fail("invalid hook surface");
        }
        let timeout = if parts.last().unwrap().is_empty() {
            Value::Null
        } else {
            json!(
                parts
                    .last()
                    .unwrap()
                    .parse::<u64>()
                    .map_err(|_| r::Error(2, "invalid hook timeout".into()))?
            )
        };
        let matcher = parts[2..parts.len() - 1].join("|");
        let entry = paths.source.join("scripts").join(parts[1]);
        let command = format!(
            "bash {} bash {}",
            r::shell(paths.source.join("scripts/lib/python-entry.sh").as_os_str())?,
            r::shell(entry.as_os_str())?
        );
        rows.push(json!({"event":parts[0],"script":parts[1],"matcher":if matcher.is_empty(){Value::Null}else{json!(matcher)},"timeout":timeout,"command":command,"fail_closed":parts[1]=="command-guard-hook.sh"}));
    }
    Ok(json!({"schema":1,"hooks_schema":schema,"root":paths.source,"surfaces":rows}))
}
pub(crate) fn command_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for ch in command.chars() {
        if matches!(ch, '\n' | '\r' | '\0') {
            return None;
        }
        if escaped {
            word.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                if q == '"' && matches!(ch, '$' | '`') {
                    return None;
                }
                word.push(ch);
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else if matches!(ch, '$' | '`' | ';' | '&' | '|' | '<' | '>' | '(' | ')') {
            return None;
        } else {
            word.push(ch);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}
fn owned_entry(command: &str, executable: &str, script: &str, paths: &Paths) -> Result<bool> {
    let Some(words) = command_words(command) else {
        return Ok(false);
    };
    let (wrapper, path) = match words.as_slice() {
        [program, path] if program == executable => (None, path),
        [bash, wrapper, program, path] if bash == "bash" && program == executable => {
            (Some(wrapper), path)
        }
        _ => return Ok(false),
    };
    let entry = Path::new(path);
    if entry.file_name().and_then(|p| p.to_str()) != Some(script) {
        return Ok(false);
    }
    let Some(root) = entry
        .parent()
        .filter(|p| p.file_name().is_some_and(|s| s == "scripts"))
        .and_then(Path::parent)
    else {
        return Ok(false);
    };
    if !root.is_absolute() {
        return Ok(false);
    }
    if let Some(wrapper) = wrapper {
        if Path::new(wrapper) != root.join("scripts/lib/python-entry.sh") {
            return Ok(false);
        }
    }
    Ok(root == paths.source
        || r::physical(root).is_ok_and(|r| r == paths.source)
        || wrapper.is_some() && !root.join("scripts/lib/python-entry.sh").exists()
        || root.join(".agents/plugins/marketplace.json").is_file()
            && root.join("scripts/install-claude-hooks.sh").is_file())
}
pub(crate) fn owned_hook(command: &str, paths: &Paths, expected: &Value) -> Result<bool> {
    for row in expected["surfaces"].as_array().unwrap() {
        if owned_entry(command, "bash", row["script"].as_str().unwrap(), paths)? {
            return Ok(true);
        }
    }
    for retired in ["tier-guard-hook.sh", "panel-direct-work-hook.sh"] {
        if owned_entry(command, "bash", retired, paths)? {
            return Ok(true);
        }
    }
    Ok(false)
}
fn claude_capability(paths: &Paths, guard: bool, flag: &AtomicBool) -> Result<bool> {
    let Some(binary) = r::find("claude", &paths.cwd) else {
        return Ok(false);
    };
    let scratch = r::Scratch::new()?;
    let home = scratch.0.join("home");
    let workspace = scratch.0.join("workspace");
    std::fs::create_dir_all(workspace.join(".claude"))?;
    std::fs::create_dir(&home)?;
    if guard {
        let fixture = json!({"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"true","onFailure":"oms-probe"}]}]}});
        let compact = serde_json::to_vec(&fixture)
            .map_err(|_| r::Error(2, "cannot encode capability fixture".into()))?;
        r::new_file(&workspace.join(".claude/settings.json"), &compact, 0o600)?;
    }
    let isolated = [
        ("HOME", home.as_os_str()),
        ("CLAUDE_CONFIG_DIR", home.as_os_str()),
        ("XDG_CONFIG_HOME", home.as_os_str()),
    ];
    let result = r::command_env(
        binary,
        &[if guard { "doctor" } else { "--help" }.into()],
        &workspace,
        None,
        flag,
        std::time::Duration::from_secs(if guard { 10 } else { 3 }),
        &isolated,
    );
    r::check(flag)?;
    let Ok(out) = result else {
        return Ok(false);
    };
    if out.0 == 124 {
        return Ok(false);
    }
    let body = [out.1, out.2].concat();
    let text = String::from_utf8_lossy(&body);
    Ok(if guard {
        text.contains("onFailure") && text.contains("\"block\"")
    } else {
        out.0 == 0 && text.contains("--autocompact <auto|tokens>")
    })
}
fn hooks(paths: &Paths, options: &Options, tx: &mut Transaction, _flag: &AtomicBool) -> Result<()> {
    if options.has("--ownership-receipt") {
        return crate::account_cleanup::hooks(paths, options, tx);
    }
    let mode = std::env::var("OMS_GUARD_FAIL_CLOSED").unwrap_or_default();
    if !options.has("--remove") && !matches!(mode.as_str(), "" | "0" | "1") {
        return r::fail("OMS_GUARD_FAIL_CLOSED must be 0 or 1");
    }
    let (enable_block, auto_compact) = if options.has("--remove") {
        (false, false)
    } else {
        options.claude_capabilities.ok_or_else(|| {
            r::Error(
                2,
                "Claude capabilities must be prepared before the install lock".into(),
            )
        })?
    };
    if !options.has("--remove") && mode == "1" && !enable_block {
        tx.warn("guard opt-in was not applied: this Claude does not validate onFailure; guards remain fail-open");
    }
    let expected = expected(paths)?;
    let mut blocked = std::collections::BTreeSet::new();
    let file = options
        .path("--settings", &paths.cwd)?
        .unwrap_or(paths.claude_settings()?);
    if options.has("--remove") && !r::exists(&file) {
        return Ok(());
    }
    let mut value = if r::exists(&file) {
        r::value(&file)?
    } else {
        json!({})
    };
    if !value.is_object() {
        return r::fail("Claude settings must be an object");
    }
    if value.get("hooks").is_none() {
        value["hooks"] = json!({});
    }
    let hooks = value["hooks"]
        .as_object_mut()
        .ok_or_else(|| r::Error(2, "Claude hooks must be an object".into()))?;
    for (event, groups) in hooks.iter_mut() {
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| r::Error(2, "hook event must be an array".into()))?;
        for group in groups.iter_mut() {
            let rows = group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| r::Error(2, "hook group must contain an array".into()))?;
            let mut kept = Vec::new();
            for row in rows.drain(..) {
                let ours = row["command"]
                    .as_str()
                    .map(|s| owned_hook(s, paths, &expected))
                    .transpose()?
                    .unwrap_or(false);
                if ours
                    && mode.is_empty()
                    && row["onFailure"] == "block"
                    && row["command"]
                        .as_str()
                        .map(|c| owned_entry(c, "bash", "command-guard-hook.sh", paths))
                        .transpose()?
                        .unwrap_or(false)
                {
                    blocked.insert(event.clone());
                }
                if !ours {
                    kept.push(row);
                }
            }
            *rows = kept;
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|a| !a.is_empty()));
    }
    hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
    if !options.has("--remove") {
        for surface in expected["surfaces"].as_array().unwrap() {
            let mut hook = json!({"type":"command","command":surface["command"]});
            if surface["fail_closed"] == true
                && (enable_block || blocked.contains(surface["event"].as_str().unwrap()))
            {
                hook["onFailure"] = json!("block");
            }
            if !surface["timeout"].is_null() {
                hook["timeout"] = surface["timeout"].clone();
            }
            let mut group = json!({"hooks":[hook]});
            if !surface["matcher"].is_null() {
                group["matcher"] = surface["matcher"].clone();
            }
            let event = surface["event"].as_str().unwrap();
            if !hooks.contains_key(event) {
                hooks.insert(event.into(), json!([]));
            }
            hooks
                .get_mut(event)
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(group);
        }
    }
    if !value["hooks"].as_object().is_none_or(|o| !o.is_empty()) {
        value.as_object_mut().unwrap().remove("hooks");
    }
    defaults(paths, &mut value, options.has("--remove"), auto_compact)?;
    let body = r::bytes(&value)?;
    if r::exists(&file) && r::read(&file, r::LIMIT)? == body {
        return Ok(());
    }
    let backup = file.with_file_name(format!(
        "{}.oh-my-setting-backup",
        file.file_name().unwrap().to_string_lossy()
    ));
    if r::exists(&file) && !r::exists(&backup) {
        tx.copy(&backup, &file)?;
    }
    tx.write(&file, &body, 0o600)
}
fn defaults(paths: &Paths, value: &mut Value, remove: bool, auto_compact: bool) -> Result<()> {
    for (key, script) in [
        ("statusLine", "claude-statusline.py"),
        ("subagentStatusLine", "claude-subagent-statusline.py"),
    ] {
        let command = format!(
            "bash {} python3 {}",
            r::shell(paths.source.join("scripts/lib/python-entry.sh").as_os_str())?,
            r::shell(paths.source.join("scripts").join(script).as_os_str())?
        );
        let ours = value[key]["command"]
            .as_str()
            .map(|c| owned_entry(c, "python3", script, paths))
            .transpose()?
            .unwrap_or(false);
        if remove {
            if ours {
                value.as_object_mut().unwrap().remove(key);
            }
        } else if value.get(key).is_none() || ours {
            value[key] = json!({"type":"command","command":command});
        }
    }
    let attribution = json!({"commit":"","pr":"","sessionUrl":false});
    if remove {
        if value["attribution"] == attribution {
            value.as_object_mut().unwrap().remove("attribution");
        }
        if let Some(models) = value
            .get_mut("modelSettings")
            .and_then(Value::as_object_mut)
        {
            if let Some(row) = models
                .get_mut("claude-opus-5-5")
                .and_then(Value::as_object_mut)
            {
                if row.get("effortLevel") == Some(&json!("high")) {
                    row.remove("effortLevel");
                }
                if row.get("autoCompactWindow") == Some(&json!(300000)) {
                    row.remove("autoCompactWindow");
                }
                if row.is_empty() {
                    models.remove("claude-opus-5-5");
                }
            }
            if models.is_empty() {
                value.as_object_mut().unwrap().remove("modelSettings");
            }
        }
    } else {
        value
            .as_object_mut()
            .unwrap()
            .entry("attribution")
            .or_insert(attribution);
        let models = value
            .as_object_mut()
            .unwrap()
            .entry("modelSettings")
            .or_insert(json!({}));
        if let Some(models) = models.as_object_mut() {
            let model = models.entry("claude-opus-5-5").or_insert(json!({}));
            if let Some(model) = model.as_object_mut() {
                model.entry("effortLevel").or_insert(json!("high"));
                if auto_compact {
                    model.entry("autoCompactWindow").or_insert(json!(300000));
                }
            }
        }
    }
    Ok(())
}
/// A dedicated managed block leaves all user TOML byte-for-byte intact. If a
/// same-named user table exists outside that block, installation refuses it.
fn toml_block(raw: &str, block: &str, table: &str) -> Result<String> {
    let begin = "# BEGIN oh-my-setting native";
    let end = "# END oh-my-setting native";
    let a = raw.match_indices(begin).collect::<Vec<_>>();
    let b = raw.match_indices(end).collect::<Vec<_>>();
    let base = match (a.as_slice(), b.as_slice()) {
        ([], []) => raw.to_owned(),
        ([(a, _)], [(b, _)]) if a < b => {
            let finish = b + end.len();
            format!("{}{}", &raw[..*a], &raw[finish..])
        }
        _ => return r::fail("malformed native TOML ownership block"),
    };
    let parsed = crate::config::toml(base.as_bytes())?;
    if base.lines().any(|s| s.trim() == table)
        || parsed["mcp_servers"].get("oh-my-setting").is_some()
    {
        return r::fail("an unmanaged TOML table already occupies this registration");
    }
    if block.is_empty() {
        return Ok(base);
    }
    Ok(format!("{}\n{begin}\n{block}\n{end}\n", base.trim_end()))
}
fn mcp(paths: &Paths, options: &Options, tx: &mut Transaction, flag: &AtomicBool) -> Result<()> {
    if !options.has("--remove") {
        crate::payload::verify(paths, flag)?.recheck()?;
    }
    let executable = paths.home.join(".local/bin/oms");
    let claude = paths.home.join(".claude.json");
    let mut value = if r::exists(&claude) {
        r::value(&claude)?
    } else {
        json!({})
    };
    if !value.is_object() {
        return r::fail("Claude configuration is not an object");
    }
    if value.get("mcpServers").is_none() {
        value["mcpServers"] = json!({});
    }
    let servers = value["mcpServers"]
        .as_object_mut()
        .ok_or_else(|| r::Error(2, "invalid MCP server table".into()))?;
    if let Some(current) = servers.get("oh-my-setting") {
        let command = current["command"].as_str().unwrap_or("");
        let args = current["args"]
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
        let native =
            command == executable.to_string_lossy() && args.as_deref() == Some(&["mcp-server"]);
        let legacy = command == "python3"
            && args.as_ref().is_some_and(|a| {
                a.len() == 1
                    && ["oms-mcp-server.py", "mcp-server.py"]
                        .iter()
                        .any(|name| Path::new(a[0]) == paths.source.join("scripts").join(name))
            });
        let cached = if args.as_deref() == Some(&["mcp-server"]) {
            paths
                .load()?
                .and_then(|row| {
                    let revision = row["native"]["revision"].as_str()?;
                    if revision.len() != 40
                        || !revision
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    {
                        return None;
                    }
                    let root = crate::receipt::owner(&row).ok()?;
                    Some(
                        root.join("rust/target/oms-native")
                            .join(revision)
                            .join("oms")
                            == Path::new(command),
                    )
                })
                .unwrap_or(false)
        } else {
            false
        };
        if !native && !legacy && !cached {
            return r::fail("refusing takeover of an unmanaged MCP registration");
        }
    }
    if options.has("--remove") {
        servers.remove("oh-my-setting");
    } else {
        servers.insert(
            "oh-my-setting".into(),
            json!({"type":"stdio","command":executable,"args":["mcp-server"]}),
        );
    }
    let config = paths.codex_config()?;
    let raw = if r::exists(&config) {
        String::from_utf8(r::read(&config, r::LIMIT)?)
            .map_err(|_| r::Error(2, "Codex configuration is not UTF-8".into()))?
    } else {
        String::new()
    };
    let section = "[mcp_servers.oh-my-setting]";
    let block = if options.has("--remove") {
        String::new()
    } else {
        format!(
            "{section}\ncommand = {}\nargs = [\"mcp-server\"]",
            serde_json::to_string(&executable)
                .map_err(|_| r::Error(2, "invalid native command path".into()))?
        )
    };
    let mut expected = crate::config::toml(raw.as_bytes())?;
    let body = toml_block(&raw, &block, section)?;
    let candidate = crate::config::toml(body.as_bytes())?;
    if options.has("--remove") {
        if let Some(servers) = expected["mcp_servers"].as_object_mut() {
            servers.remove("oh-my-setting");
        }
        if expected["mcp_servers"]
            .as_object()
            .is_some_and(|v| v.is_empty())
            && candidate.get("mcp_servers").is_none()
        {
            expected.as_object_mut().unwrap().remove("mcp_servers");
        }
    } else {
        expected["mcp_servers"]["oh-my-setting"] =
            json!({"command":executable,"args":["mcp-server"]});
    }
    if candidate != expected {
        return r::fail("MCP block edit changed unrelated TOML data; configuration retained");
    }
    if r::exists(&claude) || !options.has("--remove") {
        tx.write(&claude, &r::bytes(&value)?, 0o600)?;
    }
    if r::exists(&config) || !options.has("--remove") {
        tx.write(&config, body.as_bytes(), 0o600)?;
    }
    Ok(())
}
fn git_hook(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    if options.has("--check-protection") && !options.hook_protected {
        return r::fail("CI gate requirement was not proven; quick hook unchanged");
    }
    let raw = r::git(&paths.source, &["rev-parse", "--git-path", "hooks"], flag)?;
    let dir = PathBuf::from(raw);
    let dir = if dir.is_absolute() {
        dir
    } else {
        paths.source.join(dir)
    };
    let target = dir.join("pre-push");
    if r::exists(&target) {
        let old = r::read(&target, r::LIMIT)?;
        if !String::from_utf8_lossy(&old).contains("scripts/pre-push-check.sh") {
            return r::fail("refusing to replace an unmanaged pre-push hook");
        }
    }
    let mode = if options.has("--quick") {
        ""
    } else {
        " --full"
    };
    tx.write(&target,format!("#!/usr/bin/env bash\nset -euo pipefail\nroot=\"$(git rev-parse --show-toplevel)\"\nexec \"$root/scripts/pre-push-check.sh\"{mode} \"$@\"\n").as_bytes(),0o755)
}
pub(crate) fn prepare_hooks(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
) -> Result<Options> {
    let mut prepared = options.clone();
    if options.has("--ownership-receipt") {
        prepared.claude_capabilities = Some((false, false));
        return Ok(prepared);
    }
    if !options.preview() && !options.has("--remove") {
        let mode = std::env::var("OMS_GUARD_FAIL_CLOSED").unwrap_or_default();
        if !matches!(mode.as_str(), "" | "0" | "1") {
            return r::fail("guard mode must be 0 or 1");
        }
        prepared.claude_capabilities = Some((
            mode == "1" && claude_capability(paths, true, flag)?,
            claude_capability(paths, false, flag)?,
        ));
    }
    Ok(prepared)
}
pub(crate) fn preflight(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<()> {
    if matches!(
        options.tool.as_str(),
        "install-codex-plugin" | "install-agy-plugin"
    ) {
        return crate::plugins::preflight(paths, options, flag);
    }
    if options.tool == "install-claude-hooks" {
        if options.has("--ownership-receipt") {
            crate::account_cleanup::reservation(paths, options)?;
        }
        if !options.has("--remove") {
            match std::env::var("OMS_GUARD_FAIL_CLOSED")
                .unwrap_or_default()
                .as_str()
            {
                "" | "0" => (),
                "1" => (),
                _ => return r::fail("OMS_GUARD_FAIL_CLOSED must be 0 or 1"),
            }
        }
    }
    Ok(())
}
pub(crate) fn apply(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    match options.tool.as_str() {
        "install-claude-hooks" => hooks(paths, options, tx, flag),
        "install-mcp" => mcp(paths, options, tx, flag),
        "install-hooks" => git_hook(paths, options, tx, flag),
        "install-codex-plugin" | "install-agy-plugin" => {
            crate::plugins::apply(paths, options, tx, flag)
        }
        _ => r::fail("provider integration adapter unavailable"),
    }
}
pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if options.tool == "install-claude-hooks" && options.has("--print-expected") {
        return r::json(&expected(paths)?);
    }
    if options.tool == "install-agy-plugin" && options.has("--probe-surfaces") {
        if options.has("--remove") {
            return r::fail("--remove and --probe-surfaces are exclusive");
        }
        if !options.preview() {
            return crate::agy_surface::probe(paths, flag);
        }
    }
    if options.has("--check-protection") && !options.has("--quick") {
        return r::fail("check-protection requires quick");
    }
    if options.preview() {
        return Ok(r::text(format!(
            "would {} owned provider configuration",
            if options.has("--remove") {
                "remove"
            } else {
                "converge"
            }
        )));
    }
    r::child_guard()?;
    paths.require_owner()?;
    preflight(paths, options, flag)?;
    let prepared = if options.tool == "install-claude-hooks" {
        prepare_hooks(paths, options, flag)?
    } else {
        options.clone()
    };
    let mut prepared = prepared;
    if options.has("--check-protection") {
        let out = services.hook_protection(&paths.source, flag);
        if out.0 != 0 {
            return Err(r::Error(
                1,
                "CI gate requirement is absent or unknown; quick hook unchanged".into(),
            ));
        }
        prepared.hook_protected = true;
    }
    let options = &prepared;
    let account = options.has("--ownership-receipt");
    let _operation = if account {
        Some(crate::lifecycle_lock::acquire(paths, flag)?)
    } else {
        None
    };
    let _account = if account {
        Some(r::lock(
            &paths.config.join("provider-accounts.json"),
            &paths.cwd,
        )?)
    } else {
        None
    };
    let _guard = if account {
        None
    } else {
        Some(r::receipt_lock(paths, flag)?)
    };
    let _receipt = if account {
        r::receipt_only(paths)?
    } else {
        None
    };

    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
    apply(paths, options, &mut tx, flag)?;
    if let Some(mut row) = paths.load()? {
        let key = match options.tool.as_str() {
            "install-claude-hooks" if !account => Some("claude_hooks"),
            "install-mcp" => Some("mcp"),
            "install-codex-plugin" => Some("codex_plugin"),
            "install-agy-plugin" => Some("agy_plugin"),
            _ => None,
        };
        if let Some(key) = key {
            row["components"][key] = json!(!options.has("--remove"));
            if key == "claude_hooks" {
                row["hooks_schema"] = expected(paths)?["hooks_schema"].clone();
            }
            tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
        }
    }
    r::check(flag)?;
    let diagnostics = tx.diagnostics();
    tx.commit()?;
    let mut output = r::text(format!("{}: configuration converged", options.tool));
    output.2 = diagnostics;
    Ok(output)
}
