use crate::{
    cli::{McpToolProfile, Options},
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
    // Native registration owns its inventory; source checkouts need no shell installer.
    let surfaces = [
        ("UserPromptSubmit", "skill-router", None, None),
        ("Stop", "turn-guard", None, Some(12)),
        (
            "PostToolUseFailure",
            "fail-ledger-hook",
            Some("Bash"),
            Some(5),
        ),
        ("PostToolUse", "fail-ledger-hook", Some("Bash"), Some(5)),
        ("PreToolUse", "git-move-guard-hook", Some("Bash"), Some(5)),
        ("PreToolUse", "command-guard-hook", Some("Bash"), Some(5)),
        (
            "PreToolUse",
            "main-write-guard-hook",
            Some("Edit|Write|MultiEdit|NotebookEdit"),
            Some(5),
        ),
        (
            "PostToolUse",
            "syntax-guard-hook",
            Some("Edit|Write|MultiEdit"),
            Some(5),
        ),
        ("PreCompact", "precompact-handoff", None, Some(30)),
        ("SessionEnd", "precompact-handoff", None, Some(30)),
        ("SessionStart", "resume-hook", None, Some(10)),
        ("SessionStart", "telemetry-hook", None, Some(5)),
        ("SessionEnd", "telemetry-hook", None, Some(5)),
        (
            "Notification",
            "telemetry-hook",
            Some("permission_prompt|elicitation_dialog"),
            Some(5),
        ),
    ];
    let rows = surfaces
        .into_iter()
        .map(|(event, verb, matcher, timeout)| {
            Ok(
                json!({"event":event,"script":verb,"matcher":matcher,"timeout":timeout,
            "command":native_command(paths, verb)?,"fail_closed":verb=="command-guard-hook"}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"schema":1,"hooks_schema":13,"root":paths.source,"surfaces":rows}))
}
pub(crate) fn native_command(paths: &Paths, verb: &str) -> Result<String> {
    Ok(format!(
        "{} {verb}",
        r::shell(paths.home.join(".local/bin/oms").as_os_str())?
    ))
}
pub(crate) fn owned_native(command: &str, verb: &str, paths: &Paths) -> bool {
    command_words(command).is_some_and(|words| {
        matches!(words.as_slice(), [program, arg]
        if Path::new(program) == paths.home.join(".local/bin/oms") && arg == verb)
    })
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
    if let Some(wrapper) = wrapper
        && Path::new(wrapper) != root.join("scripts/lib/python-entry.sh")
    {
        return Ok(false);
    }
    Ok(root == paths.source
        || r::physical(root).is_ok_and(|r| r == paths.source)
        || wrapper.is_some() && !root.join("scripts/lib/python-entry.sh").exists()
        || root.join(".agents/plugins/marketplace.json").is_file()
            && root.join("scripts/install-claude-hooks.sh").is_file())
}
pub(crate) fn owned_hook(command: &str, paths: &Paths, expected: &Value) -> Result<bool> {
    for row in expected["surfaces"].as_array().unwrap() {
        let verb = row["script"].as_str().unwrap();
        if owned_native(command, verb, paths)
            || owned_entry(command, "bash", &format!("{verb}.sh"), paths)?
        {
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
                        .map(|c| {
                            Ok::<_, r::Error>(
                                owned_native(c, "command-guard-hook", paths)
                                    || owned_entry(c, "bash", "command-guard-hook.sh", paths)?,
                            )
                        })
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
        let verb = script.trim_end_matches(".py");
        let command = native_command(paths, verb)?;
        let ours = value[key]["command"]
            .as_str()
            .map(|c| {
                Ok::<_, r::Error>(
                    owned_native(c, verb, paths) || owned_entry(c, "python3", script, paths)?,
                )
            })
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
const MCP_TOOL_PROFILE: &str = "OMS_MCP_TOOL_PROFILE";

fn retained_mcp_profile(current: &Value) -> Result<Option<McpToolProfile>> {
    let Some(env) = current.get("env") else {
        return Ok(None);
    };
    let env = env
        .as_object()
        .ok_or_else(|| r::Error(2, "invalid managed MCP environment".into()))?;
    if env.is_empty() {
        return Ok(None);
    }
    if env.len() != 1 {
        return r::fail("unrecognized managed MCP environment");
    }
    let profile = env
        .get(MCP_TOOL_PROFILE)
        .and_then(Value::as_str)
        .ok_or_else(|| r::Error(2, "unrecognized managed MCP environment".into()))?;
    McpToolProfile::parse(profile).map(Some)
}

/// Preserve the host's selected discovery profile unless explicitly changed.
/// A selector never grants authority to replace an otherwise unowned entry.
pub(crate) fn managed_mcp_entry(
    paths: &Paths,
    options: &Options,
    current: Option<&Value>,
    stdio: bool,
) -> Result<Value> {
    let executable = paths.home.join(".local/bin/oms");
    if let Some(current) = current
        && !owned_mcp_entry(current, paths, &executable)?
    {
        return r::fail("refusing takeover of an unmanaged MCP registration");
    }
    let retained = current.map(retained_mcp_profile).transpose()?.flatten();
    let mut entry = json!({"command":executable,"args":["mcp-server"]});
    if stdio {
        entry["type"] = json!("stdio");
    }
    if let Some(profile) = options.mcp_tool_profile.or(retained) {
        entry["env"] = json!({MCP_TOOL_PROFILE:profile.as_str()});
    }
    Ok(entry)
}

/// Recognizes OMS-owned MCP registrations across Claude `.claude.json`, Codex
/// `config.toml`, and legacy Antigravity `mcp_config.json` manifests.
pub(crate) fn owned_mcp_entry(current: &Value, paths: &Paths, executable: &Path) -> Result<bool> {
    let Some(obj) = current.as_object() else {
        return Ok(false);
    };
    if !obj
        .keys()
        .all(|k| matches!(k.as_str(), "command" | "args" | "type" | "env"))
        || !obj.get("type").is_none_or(|v| v == "stdio")
        || retained_mcp_profile(current).is_err()
    {
        return Ok(false);
    }
    let command = obj.get("command").and_then(Value::as_str).unwrap_or("");
    let args = obj
        .get("args")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
    let native =
        command == executable.to_string_lossy() && args.as_deref() == Some(&["mcp-server"]);
    let cmd_path = Path::new(command);
    let runtime_dir = paths.data.join("python-runtime");
    let runtime_python = cmd_path.is_absolute()
        && cmd_path.starts_with(&runtime_dir)
        && cmd_path
            .file_name()
            .is_some_and(|n| n == "python" || n == "python3");
    let legacy = (command == "python3" || runtime_python)
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
                    root.join("target/oms-native").join(revision).join("oms") == cmd_path
                        || root
                            .join("rust/target/oms-native")
                            .join(revision)
                            .join("oms")
                            == cmd_path,
                )
            })
            .unwrap_or(false)
    } else {
        false
    };
    Ok(native || legacy || cached)
}

/// A dedicated managed block leaves all user TOML byte-for-byte intact. If a
/// same-named user table exists outside that block, installation refuses it
/// unless it is an unambiguous OMS-owned registration from an earlier version.
fn toml_block(
    raw: &str,
    block: &str,
    table: &str,
    paths: &Paths,
    executable: &Path,
) -> Result<String> {
    let begin = "# BEGIN oh-my-setting native";
    let end = "# END oh-my-setting native";
    let a = raw.match_indices(begin).collect::<Vec<_>>();
    let b = raw.match_indices(end).collect::<Vec<_>>();
    let mut base = match (a.as_slice(), b.as_slice()) {
        ([], []) => raw.to_owned(),
        ([(a, _)], [(b, _)]) if a < b => {
            let finish = b + end.len();
            format!("{}{}", &raw[..*a], &raw[finish..])
        }
        _ => return r::fail("malformed native TOML ownership block"),
    };
    let mut parsed = crate::config::toml(base.as_bytes())?;
    if a.is_empty()
        && b.is_empty()
        && let Some(entry) = parsed
            .get("mcp_servers")
            .and_then(|m| m.get("oh-my-setting"))
        && owned_mcp_entry(entry, paths, executable)?
    {
        let mut lines = base
            .split_inclusive('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let positions = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                matches!(
                    l.trim(),
                    "[mcp_servers.oh-my-setting]"
                        | "[mcp_servers.\"oh-my-setting\"]"
                        | "[mcp_servers.'oh-my-setting']"
                )
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if positions.len() != 1 {
            return r::fail("an unmanaged TOML table already occupies this registration");
        }
        // Older user-written registrations can express the one recognized env
        // as its own table. Remove just these proved sections, including when
        // another server sits between them; unrelated bytes remain in place.
        let mut positions = positions;
        positions.extend(lines.iter().enumerate().filter_map(|(i, l)| {
            matches!(
                l.trim(),
                "[mcp_servers.oh-my-setting.env]"
                    | "[mcp_servers.\"oh-my-setting\".env]"
                    | "[mcp_servers.'oh-my-setting'.env]"
            )
            .then_some(i)
        }));
        positions.sort_unstable();
        for h in positions.into_iter().rev() {
            let section_end = lines[h + 1..]
                .iter()
                .position(|l| {
                    let s = l.trim_start();
                    s.starts_with('[')
                        || s.starts_with("# >>>")
                        || s.starts_with("# <<<")
                        || s.starts_with("# BEGIN")
                        || s.starts_with("# END")
                })
                .map_or(lines.len(), |offset| h + 1 + offset);
            let last_kv = (h..section_end)
                .rfind(|&i| !lines[i].trim().is_empty())
                .unwrap_or(h);
            if lines[h + 1..=last_kv]
                .iter()
                .any(|l| l.trim_start().starts_with('#'))
            {
                return r::fail("an unmanaged TOML table already occupies this registration");
            }
            lines.drain(h..=last_kv);
        }
        base = lines.concat();
        parsed = crate::config::toml(base.as_bytes())?;
    }
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
    mcp_registrations(paths, options, tx, &paths.codex_config()?)
}
fn mcp_registrations(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    config: &Path,
) -> Result<()> {
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
    if let Some(current) = servers.get("oh-my-setting")
        && !owned_mcp_entry(current, paths, &executable)?
    {
        return r::fail("refusing takeover of an unmanaged MCP registration");
    }
    if options.has("--remove") {
        servers.remove("oh-my-setting");
    } else {
        let entry = managed_mcp_entry(paths, options, servers.get("oh-my-setting"), true)?;
        servers.insert("oh-my-setting".into(), entry);
    }
    let raw = if r::exists(config) {
        String::from_utf8(r::read(config, r::LIMIT)?)
            .map_err(|_| r::Error(2, "Codex configuration is not UTF-8".into()))?
    } else {
        String::new()
    };
    let mut expected = crate::config::toml(raw.as_bytes())?;
    let current = expected
        .get("mcp_servers")
        .and_then(|servers| servers.get("oh-my-setting"));
    // A marker delimits the bytes to edit; it does not authorize dropping an
    // unknown environment, launch option, or third-party command in that block.
    if let Some(current) = current
        && !owned_mcp_entry(current, paths, &executable)?
    {
        return r::fail("an unmanaged TOML table already occupies this registration");
    }
    let entry = managed_mcp_entry(paths, options, current, false)?;
    let section = "[mcp_servers.oh-my-setting]";
    let block = if options.has("--remove") {
        String::new()
    } else {
        let mut block = format!(
            "{section}\ncommand = {}\nargs = [\"mcp-server\"]",
            serde_json::to_string(&executable)
                .map_err(|_| r::Error(2, "invalid native command path".into()))?
        );
        if let Some(profile) = entry["env"][MCP_TOOL_PROFILE].as_str() {
            block.push_str(&format!("\nenv = {{ {MCP_TOOL_PROFILE} = \"{profile}\" }}"));
        }
        block
    };
    let body = toml_block(&raw, &block, section, paths, &executable)?;
    let candidate = crate::config::toml(body.as_bytes())?;
    if options.has("--remove") {
        if let Some(servers) = expected
            .get_mut("mcp_servers")
            .and_then(Value::as_object_mut)
        {
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
        expected["mcp_servers"]["oh-my-setting"] = entry;
    }
    if candidate != expected {
        return r::fail("MCP block edit changed unrelated TOML data; configuration retained");
    }
    if r::exists(&claude) || !options.has("--remove") {
        tx.write(&claude, &r::bytes(&value)?, 0o600)?;
    }
    if r::exists(config) || !options.has("--remove") {
        tx.write(config, body.as_bytes(), 0o600)?;
    }
    Ok(())
}
/// The validated hook directory, or `None` when removal has nothing to undo.
/// An explicit `--repo` selects another OMS source worktree; the receipt owner stays canonical.
fn hook_dir(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<Option<PathBuf>> {
    // The mutation helper suppresses hooks with core.hooksPath=/dev/null;
    // a read-only path query must retain Git's configured hook directory.
    let explicit = options.has("--repo");
    // Only legacy implicit withdrawal may no-op; an explicit target must be proved.
    let remove = options.has("--remove") && !explicit;
    let Some(git) = r::find("git", &paths.cwd) else {
        return if remove {
            Ok(None)
        } else {
            r::fail("git is required")
        };
    };
    let output = if explicit {
        r::command(
            git,
            &[
                "rev-parse".into(),
                "--show-toplevel".into(),
                "--git-common-dir".into(),
                "--git-path".into(),
                "hooks".into(),
            ],
            &paths.cwd,
            None,
            flag,
            std::time::Duration::from_secs(30),
        )?
    } else {
        r::command(
            git,
            &["rev-parse".into(), "--git-path".into(), "hooks".into()],
            &paths.source,
            None,
            flag,
            std::time::Duration::from_secs(30),
        )?
    };
    if output.0 != 0 {
        return if remove {
            Ok(None)
        } else {
            r::fail("cannot resolve Git hook directory")
        };
    }
    let raw = String::from_utf8(output.1)
        .map_err(|_| r::Error(2, "Git hook directory is not UTF-8".into()))?;
    if !explicit {
        let dir = PathBuf::from(raw.trim_end_matches(['\r', '\n']));
        return Ok(Some(if dir.is_absolute() {
            dir
        } else {
            paths.source.join(dir)
        }));
    }
    let base = &paths.cwd;
    let lines = raw
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let [top, common, hooks_path, ""] = lines.as_slice() else {
        return r::fail("cannot resolve Git hook directory");
    };
    let resolve = |p: &str| {
        let p = PathBuf::from(p);
        r::physical(&if p.is_absolute() { p } else { base.join(p) })
    };
    let dir = resolve(hooks_path)?;
    let top = resolve(top)?;
    if top != *base
        || !top.join("Cargo.toml").is_file()
        || !top.join("crates/oms/Cargo.toml").is_file()
    {
        return r::fail("--repo must be the root of an OMS source worktree");
    }
    if !options.has("--remove") && !top.join("scripts/check.sh").is_file() {
        return r::fail("--repo must contain scripts/check.sh");
    }
    // A shared or global core.hooksPath would write beyond this repository.
    if !dir.starts_with(resolve(common)?) && !dir.starts_with(&top) {
        return r::fail("Git hook directory is outside the selected repository");
    }
    Ok(Some(dir))
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
    let remove = options.has("--remove");
    let Some(dir) = hook_dir(paths, options, flag)? else {
        return Ok(());
    };
    // Byte-exact hook written by scripts/install-hooks.sh at e0b92359; any edit stays unmanaged.
    const OLD_PUSH: &str = "#!/usr/bin/env bash\n# Installed by scripts/install-hooks.sh — runs the repo gate before every push.\n# Bypass for an emergency with: git push --no-verify\nset -euo pipefail\nroot=\"$(git rev-parse --show-toplevel)\"\nexec \"$root/scripts/check.sh\"\n";
    let owned = |name: &str, header: &str, legacy: &str| -> Result<(PathBuf, bool)> {
        let target = dir.join(name);
        let ours = !r::exists(&target) || {
            let old = r::read(&target, r::LIMIT)?;
            let text = String::from_utf8_lossy(&old);
            text.starts_with(header)
                || !legacy.is_empty() && text.contains(legacy)
                || name == "pre-push" && text == OLD_PUSH
        };
        Ok((target, ours))
    };
    let push_header = "#!/bin/sh\n# oh-my-setting native pre-push\n";
    let merge_header = "#!/bin/sh\n# oh-my-setting native pre-merge-commit\n";
    let hooks = [
        (
            "pre-push",
            owned("pre-push", push_header, "scripts/pre-push-check.sh")?,
        ),
        (
            "pre-merge-commit",
            owned("pre-merge-commit", merge_header, "")?,
        ),
    ];
    if remove {
        // Only OMS-written hooks are withdrawn; a user's own hook stays.
        for (_, (target, ours)) in hooks {
            if ours && r::exists(&target) {
                tx.remove(&target)?;
            }
        }
        return Ok(());
    }
    for (name, (_, ours)) in &hooks {
        if !ours {
            return r::fail(format!("refusing to replace an unmanaged {name} hook"));
        }
    }
    let [(_, (push, _)), (_, (merge, _))] = hooks;
    let mode = if options.has("--quick") {
        ""
    } else {
        " --full"
    };
    let check = native_command(paths, "pre-push-check")?;
    tx.write(
        &push,
        format!("{push_header}exec {check}{mode} \"$@\"\n").as_bytes(),
        0o755,
    )?;
    tx.write(
        &merge,
        format!("{merge_header}exec {check} --pre-merge \"$@\"\n").as_bytes(),
        0o755,
    )
}
/// Provider HUD wiring that needs no plugin: Codex status line and Antigravity
/// `statusLine`. Removal touches only OMS-owned entries.
pub(crate) fn huds(
    paths: &Paths,
    tx: &mut Transaction,
    remove: bool,
    codex_plugin: bool,
) -> Result<()> {
    let codex = hud_enabled("OH_MY_SETTING_CODEX_HUD") && hud_enabled("OH_MY_SETTING_CODEX_PLUGIN");
    let agy = hud_enabled("OH_MY_SETTING_AGY_HUD");
    huds_with(
        paths,
        tx,
        remove,
        (codex && !codex_plugin, agy),
        (&paths.codex_home()?, &paths.codex_config()?),
    )
}
fn huds_with(
    paths: &Paths,
    tx: &mut Transaction,
    remove: bool,
    (codex, agy): (bool, bool),
    (codex_home, codex_config): (&Path, &Path),
) -> Result<()> {
    if remove {
        crate::settings::codex(paths, tx, codex_config, true)?;
        return crate::plugins::agy_statusline(paths, tx, true);
    }
    if codex
        && r::exists(codex_home)
        && let Err(e) = crate::settings::codex(paths, tx, codex_config, false)
    {
        tx.warn(&format!("Codex HUD not installed: {}", e.1));
    }
    if agy && r::exists(&paths.home.join(".gemini/antigravity-cli")) {
        crate::plugins::agy_statusline(paths, tx, false)?;
    }
    Ok(())
}
pub(crate) fn hud_enabled(var: &str) -> bool {
    std::env::var_os(var).is_none_or(|v| v != "0")
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
            "would {} owned provider configuration{}",
            if options.has("--remove") {
                "remove"
            } else {
                "converge"
            },
            options
                .mcp_tool_profile
                .map_or_else(String::new, |profile| {
                    format!(" with MCP tool profile {}", profile.as_str())
                })
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
        // An explicit target must prove its own protection, after validation and before mutation.
        let target = if options.tool == "install-hooks" && options.has("--repo") {
            hook_dir(paths, options, flag)?;
            &paths.cwd
        } else {
            &paths.source
        };
        let out = services.hook_protection(target, flag);
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
            if key == "agy_plugin" && options.has("--remove") {
                // An explicit withdrawal must survive auto-enable on the next install.
                row["component_modes"]["agy_plugin"] = json!("0");
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mcp_profiles_survive_reapply_migration_and_owned_removal() {
        let scratch = r::Scratch::new().unwrap();
        let paths = Paths {
            home: scratch.0.join("home"),
            source: scratch.0.join("source"),
            receipt: scratch.0.join("config/install.json"),
            config: scratch.0.join("config"),
            state: scratch.0.join("state"),
            data: scratch.0.join("data"),
            cwd: scratch.0.clone(),
        };
        let claude = paths.home.join(".claude.json");
        let codex = paths.home.join(".codex/config.toml");
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        let user = "# keep user formatting\ncustom = true\n";
        fs::write(&codex, user).unwrap();
        fs::write(&claude, br#"{"custom":true}"#).unwrap();
        let converge = |args: &[&str]| -> Result<()> {
            let options = crate::cli::parse(
                "install-mcp",
                &args
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>(),
            )?;
            let mut tx = Transaction::begin(&paths)?;
            // Exercise the production registration writer after its separate
            // native-payload gate; no provider or build runs in this fixture.
            mcp_registrations(&paths, &options, &mut tx, &codex)?;
            tx.commit()
        };
        let entries = || {
            (
                r::value(&claude).unwrap()["mcpServers"]["oh-my-setting"].clone(),
                crate::config::read_toml(&codex).unwrap()["mcp_servers"]["oh-my-setting"].clone(),
            )
        };
        let bytes = || (fs::read(&claude).unwrap(), fs::read(&codex).unwrap());
        converge(&[]).unwrap();
        for entry in [entries().0, entries().1] {
            assert!(entry.get("env").is_none(), "legacy default stays implicit");
        }
        for profile in ["full", "core"] {
            converge(&["--mcp-tool-profile", profile]).unwrap();
            for entry in [entries().0, entries().1] {
                assert_eq!(entry["env"][MCP_TOOL_PROFILE], profile);
            }
            let saved = bytes();
            converge(&[]).unwrap();
            assert_eq!(bytes(), saved, "reapply must preserve {profile}");
        }

        let mut selected = r::value(&claude).unwrap();
        selected["mcpServers"]["oh-my-setting"]["env"][MCP_TOOL_PROFILE] = json!("full");
        fs::write(&claude, r::bytes(&selected).unwrap()).unwrap();
        converge(&[]).unwrap();
        assert_eq!(entries().0["env"][MCP_TOOL_PROFILE], "full");
        assert_eq!(entries().1["env"][MCP_TOOL_PROFILE], "core");

        // Adopt only the exact legacy command while retaining its selection.
        // The nested env table may be separated from the server by user data.
        let legacy = paths.source.join("scripts/oms-mcp-server.py");
        selected["mcpServers"]["oh-my-setting"]["command"] = json!("python3");
        selected["mcpServers"]["oh-my-setting"]["args"] = json!([legacy]);
        fs::write(&claude, r::bytes(&selected).unwrap()).unwrap();
        let sibling = "[mcp_servers.other]\ncommand = \"user-mcp\"\nargs = []\n";
        fs::write(
            &codex,
            format!(
                "{user}\n[mcp_servers.'oh-my-setting']\ncommand = \"python3\"\nargs = [{}]\n\n{sibling}\n[mcp_servers.'oh-my-setting'.env]\n{MCP_TOOL_PROFILE} = \"full\"\n",
                json!(legacy)
            ),
        )
        .unwrap();
        converge(&[]).unwrap();
        for entry in [entries().0, entries().1] {
            assert_eq!(entry["command"], json!(paths.home.join(".local/bin/oms")));
            assert_eq!(entry["args"], json!(["mcp-server"]));
            assert_eq!(entry["env"][MCP_TOOL_PROFILE], "full");
        }
        let raw = fs::read_to_string(&codex).unwrap();
        assert!(raw.starts_with(user));
        assert!(raw.contains(sibling));
        let saved = bytes();
        converge(&[]).unwrap();
        assert_eq!(bytes(), saved);

        // A selector cannot discard unrecognized settings. In particular, an
        // invalid Codex block must leave the already-read Claude file intact.
        let native = json!(paths.home.join(".local/bin/oms"));
        for env in [
            "env = { OMS_MCP_TOOL_PROFILE = \"unknown\" }",
            "env = { OMS_MCP_TOOL_PROFILE = 1 }",
            "env = { OMS_MCP_TOOL_PROFILE = \"full\", USER_CUSTOM = \"1\" }",
            "env = { USER_CUSTOM = \"1\" }",
            "env = []",
        ] {
            for marked in [false, true] {
                let block = format!(
                    "[mcp_servers.oh-my-setting]\ncommand = {native}\nargs = [\"mcp-server\"]\n{env}\n"
                );
                fs::write(
                    &codex,
                    if marked {
                        format!("{user}\n# BEGIN oh-my-setting native\n{block}# END oh-my-setting native\n")
                    } else {
                        format!("{user}\n{block}")
                    },
                )
                .unwrap();
                let before = bytes();
                assert!(converge(&["--mcp-tool-profile", "core"]).is_err());
                assert_eq!(bytes(), before);
                assert!(converge(&["--remove"]).is_err());
                assert_eq!(bytes(), before);
            }
        }
        fs::write(&codex, &saved.1).unwrap();
        let valid = r::value(&claude).unwrap();
        for env in [
            json!({MCP_TOOL_PROFILE:"unknown"}),
            json!({MCP_TOOL_PROFILE:1}),
            json!({MCP_TOOL_PROFILE:"full","USER_CUSTOM":"1"}),
            json!({"USER_CUSTOM":"1"}),
            json!(null),
            json!([]),
        ] {
            let mut changed = valid.clone();
            changed["mcpServers"]["oh-my-setting"]["env"] = env;
            fs::write(&claude, r::bytes(&changed).unwrap()).unwrap();
            let before = bytes();
            assert!(converge(&["--mcp-tool-profile", "full"]).is_err());
            assert_eq!(bytes(), before);
        }
        let mut foreign = valid;
        foreign["mcpServers"]["oh-my-setting"]["command"] = json!("other-mcp");
        fs::write(&claude, r::bytes(&foreign).unwrap()).unwrap();
        let before = bytes();
        assert!(converge(&["--mcp-tool-profile", "full"]).is_err());
        assert_eq!(bytes(), before);
        fs::write(&claude, &saved.0).unwrap();
        let foreign_block = format!(
            "{user}\n# BEGIN oh-my-setting native\n[mcp_servers.oh-my-setting]\ncommand = \"other-mcp\"\nargs = [\"mcp-server\"]\nenv = {{ {MCP_TOOL_PROFILE} = \"full\" }}\n# END oh-my-setting native\n"
        );
        fs::write(&codex, foreign_block).unwrap();
        let before = bytes();
        assert!(converge(&["--mcp-tool-profile", "full"]).is_err());
        assert_eq!(bytes(), before);
        fs::write(&codex, &saved.1).unwrap();

        converge(&["--remove"]).unwrap();
        assert!(entries().0.is_null());
        assert!(entries().1.is_null());
        assert_eq!(r::value(&claude).unwrap()["custom"], true);
        let raw = fs::read_to_string(&codex).unwrap();
        assert!(raw.starts_with(user));
        assert!(raw.contains(sibling));
        assert!(!raw.contains(MCP_TOOL_PROFILE));
    }

    #[test]
    fn hud_wiring_installs_when_hosts_exist_and_removes_only_owned_entries() {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-huds-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = Dir(root.clone());
        let (agy, codex) = (
            root.join("home/.gemini/antigravity-cli"),
            root.join("home/.codex"),
        );
        fs::create_dir_all(&agy).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        let paths = Paths {
            home: root.join("home"),
            source: root.join("source"),
            receipt: root.join("receipt"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cwd: root.clone(),
        };
        let config = codex.join("config.toml");
        let bare = "[tui]\nstatus_line = [\"model\"]\n";
        fs::write(&config, bare).unwrap();
        let apply = |remove: bool, on: (bool, bool)| {
            let mut tx = Transaction::begin(&paths).unwrap();
            huds_with(&paths, &mut tx, remove, on, (&codex, &config)).unwrap();
            tx.commit().unwrap();
        };
        apply(false, (false, false));
        assert_eq!(fs::read_to_string(&config).unwrap(), bare);
        assert!(!agy.join("settings.json").exists());
        apply(false, (true, true));
        assert!(
            fs::read_to_string(&config)
                .unwrap()
                .contains("# displaced:")
        );
        assert!(
            fs::read_to_string(agy.join("settings.json"))
                .unwrap()
                .contains("agy-statusline")
        );
        apply(true, (false, false));
        assert_eq!(fs::read_to_string(&config).unwrap(), bare);
        assert!(
            !fs::read_to_string(agy.join("settings.json"))
                .unwrap()
                .contains("statusLine")
        );
        drop(dir);
    }
}
