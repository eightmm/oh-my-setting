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
fn owned_hook(command: &str, paths: &Paths) -> Result<bool> {
    let rows = expected(paths)?;
    for row in rows["surfaces"].as_array().unwrap() {
        let script = row["script"].as_str().unwrap();
        for command_form in [
            format!(
                "bash {}",
                r::shell(paths.source.join("scripts").join(script).as_os_str())?
            ),
            format!(
                "bash {} bash {}",
                r::shell(paths.source.join("scripts/lib/python-entry.sh").as_os_str())?,
                r::shell(paths.source.join("scripts").join(script).as_os_str())?
            ),
        ] {
            if command == command_form {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn hooks(paths: &Paths, options: &Options, tx: &mut Transaction) -> Result<()> {
    if options.has("--ownership-receipt") {
        return r::fail(
            "account hook installation requires the provider-account reservation adapter",
        );
    }
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
    for groups in hooks.values_mut() {
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
                if !row["command"]
                    .as_str()
                    .map(|s| owned_hook(s, paths))
                    .transpose()?
                    .unwrap_or(false)
                {
                    kept.push(row);
                }
            }
            *rows = kept;
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|a| !a.is_empty()));
    }
    hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
    if !options.has("--remove") {
        for surface in expected(paths)?["surfaces"].as_array().unwrap() {
            let mut hook = json!({"type":"command","command":surface["command"]});
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
    if base.lines().any(|s| s.trim() == table) {
        return r::fail("an unmanaged TOML table already occupies this registration");
    }
    if block.is_empty() {
        return Ok(base);
    }
    Ok(format!("{}\n{begin}\n{block}\n{end}\n", base.trim_end()))
}
fn mcp(paths: &Paths, options: &Options, tx: &mut Transaction, flag: &AtomicBool) -> Result<()> {
    let executable = crate::payload::verify(paths, flag)?.executable;
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
        if command != executable.to_string_lossy()
            && current["args"].as_array().is_none_or(|a| {
                !a.iter().any(|v| {
                    v.as_str()
                        .is_some_and(|s| Path::new(s) == paths.source.join("scripts/mcp-server.py"))
                })
            })
        {
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
    if r::exists(&claude) || !options.has("--remove") {
        tx.write(&claude, &r::bytes(&value)?, 0o600)?;
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
    let body = toml_block(&raw, &block, section)?;
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
    if options.has("--check-protection") {
        return r::fail("quick hook protection requires the CI-status protection adapter");
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
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.tool == "install-claude-hooks" && options.has("--print-expected") {
        return r::json(&expected(paths)?);
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
    // Provider plugin registration has provider-owned external state. Refuse
    // unsupported adapters before opening a transaction or editing files.
    if matches!(
        options.tool.as_str(),
        "install-codex-plugin" | "install-agy-plugin"
    ) {
        return r::fail(format!(
            "{} requires the native provider plugin adapter; no provider state changed",
            options.tool
        ));
    }
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
    match options.tool.as_str() {
        "install-claude-hooks" => hooks(paths, options, &mut tx)?,
        "install-mcp" => mcp(paths, options, &mut tx, flag)?,
        "install-hooks" => git_hook(paths, options, &mut tx, flag)?,
        _ => return r::fail("unknown integration installer"),
    }
    if let Some(mut row) = paths.load()? {
        let key = match options.tool.as_str() {
            "install-claude-hooks" => Some("claude_hooks"),
            "install-mcp" => Some("mcp"),
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
    tx.commit()?;
    Ok(r::text(format!(
        "{}: configuration converged",
        options.tool
    )))
}
