use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::{self, Transaction},
};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

const PLUGIN_ID: &str = "oh-my-setting@oh-my-setting-local";
const MARKETPLACE: &str = "oh-my-setting-local";
fn codex(paths: &Paths, args: &[&str], flag: &AtomicBool) -> Result<crate::Output> {
    let binary = r::find("codex", &paths.cwd)
        .ok_or_else(|| r::Error(2, "Codex is unavailable for plugin inventory".into()))?;
    r::command(
        binary,
        &args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>(),
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(120),
    )
}
struct Inventory {
    installed: bool,
    root: Option<PathBuf>,
    shared: bool,
}

/// Names only the marketplaces Codex reports as broken; its paths and reasons
/// stay withheld. At most three hints, so they survive the update gate's
/// four-line bound together with the error line.
fn inventory_failure(stderr: &[u8]) -> r::Error {
    let mut names: Vec<String> = Vec::new();
    for line in r::plain_lines(&String::from_utf8_lossy(stderr)) {
        if let Some((name, _)) = line
            .trim()
            .strip_prefix("- `")
            .and_then(|rest| rest.split_once("` at "))
            && r::safe_name(name)
            && !name.starts_with('-')
            && names.len() < 3
            && !names.iter().any(|seen| seen == name)
        {
            names.push(name.into());
        }
    }
    let mut message = "Codex plugin inventory unavailable; no configuration changed".to_owned();
    if !names.is_empty() {
        message.push_str(&format!("; failed marketplace(s): {}", names.join(", ")));
        for name in &names {
            message.push_str(&format!("\nhint: codex plugin marketplace remove {name}"));
        }
    }
    r::Error(2, message)
}

fn inventory(paths: &Paths, flag: &AtomicBool) -> Result<Inventory> {
    let out = codex(paths, &["plugin", "list", "--json"], flag)?;
    if out.0 != 0 {
        return Err(inventory_failure(&out.2));
    }
    let value = r::parse(&out.1)?;
    let rows = value["installed"]
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid Codex plugin inventory".into()))?;
    let installed = rows
        .iter()
        .any(|row| row["pluginId"] == PLUGIN_ID && row["installed"] == true);
    let out = codex(paths, &["plugin", "marketplace", "list"], flag)?;
    if out.0 != 0 {
        return r::fail("Codex marketplace inventory unavailable");
    }
    let text = std::str::from_utf8(&out.1)
        .map_err(|_| r::Error(2, "invalid Codex marketplace inventory".into()))?;
    let root = text.lines().find_map(|line| {
        let line = line.trim();
        let end = line.find(char::is_whitespace)?;
        (line.get(..end) == Some(MARKETPLACE)).then(|| PathBuf::from(line[end..].trim()))
    });
    let shared = rows.iter().any(|row| {
        row["installed"] == true
            && row["pluginId"]
                .as_str()
                .and_then(|id| id.rsplit_once('@'))
                .is_some_and(|(name, market)| name != "oh-my-setting" && market == MARKETPLACE)
    });
    Ok(Inventory {
        installed,
        root,
        shared,
    })
}
pub(crate) fn cache(paths: &Paths) -> Result<PathBuf> {
    let source = paths.source.join("plugins/oh-my-setting");
    let version = r::value(&source.join(".codex-plugin/plugin.json"))?["version"]
        .as_str()
        .filter(|s| r::safe_name(s) && !matches!(*s, "." | ".."))
        .ok_or_else(|| r::Error(2, "invalid Codex plugin version".into()))?
        .to_owned();
    Ok(paths
        .codex_home()?
        .join("plugins/cache/oh-my-setting-local/oh-my-setting")
        .join(version))
}
pub(crate) fn codex_hooks(paths: &Paths) -> Result<serde_json::Value> {
    let mut hooks = json!({"hooks":{}});
    for (event, action, matcher, timeout) in [
        ("UserPromptSubmit", "skill-router", None, 5),
        ("Stop", "turn-guard", None, 12),
        ("PreCompact", "precompact-handoff codex", None, 30),
        ("SessionStart", "resume-hook", None, 10),
        ("SessionStart", "telemetry-hook", None, 5),
        ("PostToolUse", "syntax-guard-hook", Some("apply_patch"), 5),
        ("SessionEnd", "telemetry-hook", None, 3),
    ] {
        let mut group = json!({"hooks":[{"type":"command","command":format!("{} {action}",
            crate::integrations::native_command(paths, "harness-hook")?),"timeout":timeout}]});
        if let Some(matcher) = matcher {
            group["matcher"] = json!(matcher);
        }
        hooks["hooks"]
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert(json!([]))
            .as_array_mut()
            .unwrap()
            .push(group);
    }
    Ok(hooks)
}
fn native_codex(paths: &Paths) -> Result<r::Scratch> {
    let source = paths.source.join("plugins/oh-my-setting");
    let stage = r::Scratch::new()?;
    let hooks = codex_hooks(paths)?;
    std::fs::create_dir(stage.0.join(".codex-plugin"))?;
    r::new_file(
        &stage.0.join(".codex-plugin/plugin.json"),
        &r::read(&source.join(".codex-plugin/plugin.json"), r::LIMIT)?,
        0o600,
    )?;
    r::new_file(&stage.0.join("hooks.json"), &r::bytes(&hooks)?, 0o600)?;
    Ok(stage)
}
fn owned_cache(paths: &Paths, cache: &std::path::Path, deleted: bool) -> Result<()> {
    let base = paths.codex_home()?;
    let mut ancestor = cache;
    while ancestor != base {
        if r::exists(ancestor) {
            let metadata = std::fs::symlink_metadata(ancestor)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return r::fail("Codex plugin cache must use physical directories");
            }
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| r::Error(2, "Codex cache escaped its home".into()))?;
    }
    if !r::exists(cache) {
        return Ok(());
    }
    let actual = crate::fingerprint::tree(cache, false)?;
    if actual == crate::fingerprint::tree(&paths.source.join("plugins/oh-my-setting"), false)? {
        return Ok(());
    }
    let root = String::from_utf8(r::read(
        &cache.join(".oh-my-setting-source-root"),
        r::LIMIT,
    )?)
    .map_err(|_| r::Error(2, "invalid Codex cache ownership".into()))?;
    let recorded = String::from_utf8(r::read(&cache.join(".oh-my-setting-source-sha256"), 128)?)
        .map_err(|_| r::Error(2, "invalid Codex cache digest".into()))?;
    let root = r::physical(&PathBuf::from(root.trim()))?;
    let owner = paths
        .load()?
        .as_ref()
        .map(crate::receipt::owner)
        .transpose()?;
    if (root != paths.source && owner.as_ref() != Some(&root) && !(deleted && !root.exists()))
        || actual != recorded.trim()
    {
        return r::fail("modified or foreign Codex plugin cache; refresh refused");
    }
    Ok(())
}
fn config(paths: &Paths) -> Result<serde_json::Value> {
    let file = paths.codex_home()?.join("config.toml");
    if r::exists(&file) {
        crate::config::read_toml(&file)
    } else {
        Ok(json!({}))
    }
}
fn unrelated(mut value: serde_json::Value) -> serde_json::Value {
    for (table, name) in [("plugins", PLUGIN_ID), ("marketplaces", MARKETPLACE)] {
        if let Some(rows) = value
            .get_mut(table)
            .and_then(serde_json::Value::as_object_mut)
        {
            rows.remove(name);
            if rows.is_empty() {
                value.as_object_mut().unwrap().remove(table);
            }
        }
    }
    value
}
/// Read-only inventory and generated-cache comparison, using the installer's
/// existing bounded CLI and ownership checks without refreshing the plugin.
pub(crate) fn codex_healthy(paths: &Paths, flag: &AtomicBool) -> Result<bool> {
    let Inventory {
        installed, root, ..
    } = inventory(paths, flag)?;
    if !installed || root.as_ref().and_then(|p| r::physical(p).ok()).as_ref() != Some(&paths.source)
    {
        return Ok(false);
    }
    let destination = cache(paths)?;
    if !destination.is_dir() {
        return Ok(false);
    }
    owned_cache(paths, &destination, false)?;
    let native = native_codex(paths)?;
    Ok(crate::fingerprint::tree(&destination, false)?
        == crate::fingerprint::tree(&native.0, false)?)
}
pub(crate) fn preflight(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<()> {
    if options.tool == "install-agy-plugin" {
        return Ok(());
    }
    let before = config(paths)?;
    let settings = paths.codex_config()?;
    if settings != paths.codex_home()?.join("config.toml") && r::exists(&settings) {
        let other = crate::config::read_toml(&settings)?;
        if other["plugins"].get(PLUGIN_ID).is_some()
            || other["marketplaces"].get(MARKETPLACE).is_some()
        {
            return r::fail(
                "Codex CLI edits its home config; plugin tables in a config override require explicit reconciliation",
            );
        }
    }
    if !options.has("--remove") {
        let manifest = std::env::var_os("OMS_CODEX_MARKETPLACE_FILE")
            .map(PathBuf::from)
            .unwrap_or(paths.source.join(".agents/plugins/marketplace.json"));
        if r::physical(&manifest)? != paths.source.join(".agents/plugins/marketplace.json")
            || r::value(&manifest)?["name"] != MARKETPLACE
        {
            return r::fail("Codex marketplace must match the reviewed source manifest");
        }
    }
    if r::find("codex", &paths.cwd).is_none() {
        if options.has("--remove")
            && before["plugins"].get(PLUGIN_ID).is_none()
            && before["marketplaces"].get(MARKETPLACE).is_none()
        {
            return Ok(());
        }
        return r::fail("Codex CLI is required to inspect and change its plugin registration");
    }
    let Inventory {
        installed, root, ..
    } = inventory(paths, flag)?;
    if before["plugins"].get(PLUGIN_ID).is_some() && !installed
        || before["marketplaces"].get(MARKETPLACE).is_some() && root.is_none()
    {
        return r::fail("unmanaged Codex plugin or marketplace table; takeover refused");
    }
    if let Some(root) = root.filter(|_| !options.has("--remove")) {
        let root = r::physical(&root)?;
        let manifest = paths.source.join(".agents/plugins/marketplace.json");
        if root != paths.source
            && root != manifest
            && root != paths.source.join(".agents/plugins")
            && !(root.join(".agents/plugins/marketplace.json").is_file()
                && root.join("rules/global-AGENTS.md").is_file())
        {
            return r::fail("foreign Codex marketplace; takeover refused");
        }
    }
    owned_cache(paths, &cache(paths)?, options.has("--remove"))?;
    if installed {
        let root = paths
            .codex_home()?
            .join("plugins/cache/oh-my-setting-local/oh-my-setting");
        if r::exists(&root) {
            for entry in std::fs::read_dir(root)? {
                owned_cache(paths, &entry?.path(), options.has("--remove"))?;
            }
        }
    }
    Ok(())
}
fn apply_codex(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    let remove = options.has("--remove");
    if remove {
        crate::settings::codex(paths, tx, &paths.codex_config()?, true)?;
    }
    if r::find("codex", &paths.cwd).is_none() {
        return Ok(());
    }
    let Inventory {
        installed,
        root,
        shared,
    } = inventory(paths, flag)?;
    let before = config(paths)?;
    let targets = [
        paths.codex_home()?.join("config.toml"),
        paths
            .codex_home()?
            .join("plugins/cache/oh-my-setting-local/oh-my-setting"),
        paths
            .codex_home()?
            .join("plugins/marketplaces/oh-my-setting-local"),
    ];
    let start = tx.prepare_external(&targets)?;
    let result = (|| -> Result<()> {
        let mut commands: Vec<Vec<String>> = Vec::new();
        if remove {
            if installed {
                commands.push(vec!["plugin".into(), "remove".into(), PLUGIN_ID.into()]);
            }
            if root.is_some() && !shared {
                commands.push(vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "remove".into(),
                    MARKETPLACE.into(),
                ]);
            }
        } else {
            let destination = cache(paths)?;
            let native = native_codex(paths)?;
            let stale = !r::exists(&destination)
                || crate::fingerprint::tree(&destination, false)?
                    != crate::fingerprint::tree(&native.0, false)?;
            let desired = std::env::var_os("OMS_CODEX_MARKETPLACE_ROOT")
                .map(PathBuf::from)
                .unwrap_or(paths.source.clone());
            let desired = r::physical(&desired)?;
            if desired != paths.source {
                return r::fail(
                    "Codex marketplace override must resolve to the reviewed source checkout",
                );
            }
            let moved = root
                .as_ref()
                .is_some_and(|p| r::physical(p).ok().as_ref() != Some(&desired));
            if installed && (moved || stale) {
                commands.push(vec!["plugin".into(), "remove".into(), PLUGIN_ID.into()]);
            }
            if moved {
                commands.push(vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "remove".into(),
                    MARKETPLACE.into(),
                ]);
            }
            if root
                .as_ref()
                .is_none_or(|p| r::physical(p).ok().as_ref() != Some(&desired))
            {
                commands.push(vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "add".into(),
                    desired
                        .to_str()
                        .ok_or_else(|| {
                            r::Error(2, "Codex marketplace requires a UTF-8 path".into())
                        })?
                        .into(),
                ]);
            }
            if !installed || moved || stale {
                commands.push(vec!["plugin".into(), "add".into(), PLUGIN_ID.into()]);
            }
        }
        for args in commands {
            let out = codex(
                paths,
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
                flag,
            )?;
            if out.0 != 0 {
                return Err(r::Error(
                    out.0,
                    "Codex plugin transaction failed; provider output withheld".into(),
                ));
            }
        }
        Ok(())
    })();
    tx.finish_external(start)?;
    result?;
    if unrelated(before) != unrelated(config(paths)?) {
        return r::fail("Codex CLI changed unrelated settings; transaction refused");
    }
    let after = inventory(paths, flag)?;
    if remove {
        if after.installed || after.root != root.filter(|_| shared) || after.shared != shared {
            return r::fail(
                "Codex CLI did not remove its hook plugin or preserve the expected marketplace state",
            );
        }
        return Ok(());
    }
    if !after.installed || after.root.is_none() {
        return r::fail("Codex CLI did not register its plugin and marketplace");
    }
    let cache = cache(paths)?;
    owned_cache(paths, &cache, false)?;
    let native = native_codex(paths)?;
    let expected = crate::fingerprint::tree(&native.0, false)?;
    if crate::fingerprint::tree(&cache, false)? != expected {
        tx.publish_external(&cache, &native.0)?;
    }
    tx.write(
        &cache.join(".oh-my-setting-source-root"),
        format!(
            "{}\n",
            paths
                .source
                .to_str()
                .ok_or_else(|| r::Error(2, "plugin provenance requires UTF-8".into()))?
        )
        .as_bytes(),
        0o600,
    )?;
    tx.write(
        &cache.join(".oh-my-setting-source-sha256"),
        format!("{expected}\n").as_bytes(),
        0o600,
    )?;
    crate::settings::codex(paths, tx, &paths.codex_config()?, false)
}
fn owned_agy_statusline(command: &str) -> bool {
    crate::integrations::command_words(command).is_some_and(|words| {
        words.iter().any(|w| {
            std::path::Path::new(w)
                .file_name()
                .is_some_and(|n| n == "agy-statusline" || n == "agy-statusline.sh")
        })
    })
}
/// Antigravity `statusLine` wiring. A missing or OMS-owned entry converges on
/// the native HUD; any other user entry survives install and remove.
pub(crate) fn agy_statusline(paths: &Paths, tx: &mut Transaction, remove: bool) -> Result<()> {
    let file = paths.home.join(".gemini/antigravity-cli/settings.json");
    if r::exists(&file) {
        // Existing settings are read first: a malformed file is never rewritten.
    } else if remove {
        return Ok(());
    }
    let mut value = if r::exists(&file) {
        match r::value(&file) {
            Ok(v) if v.is_object() => v,
            _ => {
                tx.warn("Antigravity settings are not a JSON object; statusLine left unchanged");
                return Ok(());
            }
        }
    } else {
        json!({})
    };
    let ours = value["statusLine"]["command"]
        .as_str()
        .is_some_and(owned_agy_statusline);
    if remove {
        if !ours {
            return Ok(());
        }
        value.as_object_mut().unwrap().remove("statusLine");
    } else if value.get("statusLine").is_none() || ours {
        value["statusLine"] = json!({
            "type":"command",
            "command":crate::integrations::native_command(paths, "agy-statusline")?,
            "enabled":true,"padding":0,"stack_with_default":false
        });
    } else {
        return Ok(());
    }
    let body = r::bytes(&value)?;
    if r::exists(&file) {
        if r::read(&file, r::LIMIT)? == body {
            return Ok(());
        }
        let backup = file.with_file_name("settings.json.oh-my-setting-backup");
        if !r::exists(&backup) {
            tx.copy(&backup, &file)?;
        }
    }
    tx.write(&file, &body, 0o600)
}
/// Recognizes pre-native Antigravity plugin directories written before marker
/// receipts existed while rejecting foreign or tampered directories.
fn owned_legacy_agy(
    paths: &Paths,
    source: &std::path::Path,
    target: &std::path::Path,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::symlink_metadata(target) else {
        return Ok(false);
    };
    if !m.is_dir() || m.file_type().is_symlink() {
        return Ok(false);
    }
    let mut seen = std::collections::BTreeSet::new();
    for row in std::fs::read_dir(target)? {
        let row = row?;
        let em = std::fs::symlink_metadata(row.path())?;
        let Some(name) = row.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        if !em.is_file()
            || em.file_type().is_symlink()
            || em.nlink() != 1
            || !matches!(
                name.as_str(),
                "plugin.json" | "mcp_config.json" | "hooks.json"
            )
        {
            return Ok(false);
        }
        seen.insert(name);
    }
    if !seen.contains("plugin.json") || !seen.contains("mcp_config.json") {
        return Ok(false);
    }
    let template_path = source.join("plugin.json");
    if !r::exists(&template_path) {
        return Ok(false);
    }
    let template = r::value(&template_path)?;
    let Ok(plugin) = r::value(&target.join("plugin.json")) else {
        return Ok(false);
    };
    let Some(plugin_obj) = plugin.as_object() else {
        return Ok(false);
    };
    if !plugin_obj
        .keys()
        .all(|k| matches!(k.as_str(), "name" | "version" | "description"))
        || plugin["name"] != "oh-my-setting"
        || !plugin["version"].as_str().is_some_and(|s| !s.is_empty())
        || plugin["description"] != template["description"]
    {
        return Ok(false);
    }
    let Ok(mcp) = r::value(&target.join("mcp_config.json")) else {
        return Ok(false);
    };
    let Some(mcp_obj) = mcp.as_object() else {
        return Ok(false);
    };
    if mcp_obj.len() != 1 || !mcp_obj.contains_key("mcpServers") {
        return Ok(false);
    }
    let Some(servers) = mcp["mcpServers"].as_object() else {
        return Ok(false);
    };
    if servers.len() != 1 {
        return Ok(false);
    }
    let Some(entry) = servers.get("oh-my-setting") else {
        return Ok(false);
    };
    if !crate::integrations::owned_mcp_entry(entry, paths, &paths.home.join(".local/bin/oms"))? {
        return Ok(false);
    }
    if seen.contains("hooks.json") {
        let Ok(hooks) = r::value(&target.join("hooks.json")) else {
            return Ok(false);
        };
        let Some(hooks_obj) = hooks.as_object() else {
            return Ok(false);
        };
        if !hooks_obj.keys().all(|k| k == "oh-my-setting") {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn apply(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<()> {
    preflight(paths, options, flag)?;
    let remove = options.has("--remove");
    if options.tool == "install-codex-plugin" {
        return apply_codex(paths, options, tx, flag);
    }
    r::check(flag)?;
    let source = paths.source.join("plugins/antigravity");
    let target = paths.home.join(".gemini/config/plugins/oh-my-setting");
    let marker = crate::install::marker(&target);
    let mcp = agy_mcp(paths, options, &source, &target)?;
    if remove {
        tx.remove(&target)?;
        if r::exists(&marker) {
            tx.remove(&marker)?;
        }
        return agy_statusline(paths, tx, true);
    }
    crate::payload::verify(paths, flag)?.recheck()?;
    write_agy_plugin(paths, tx, &source, &target, &mcp)
}

fn agy_mcp(
    paths: &Paths,
    options: &Options,
    source: &std::path::Path,
    target: &std::path::Path,
) -> Result<serde_json::Value> {
    let marker = crate::install::marker(target);
    if (r::exists(target) || r::exists(&marker))
        && !crate::install::owned(source, target)?
        && (r::exists(&marker) || !owned_legacy_agy(paths, source, target)?)
    {
        return r::fail("unmanaged or modified Antigravity plugin; takeover refused");
    }
    let file = target.join("mcp_config.json");
    let current = if r::exists(&file) {
        let mcp = r::value(&file)?;
        if !mcp.as_object().is_some_and(|o| o.len() == 1)
            || !mcp["mcpServers"]
                .as_object()
                .is_some_and(|o| o.len() == 1 && o.contains_key("oh-my-setting"))
        {
            return r::fail("unmanaged Antigravity MCP configuration; takeover refused");
        }
        Some(mcp["mcpServers"]["oh-my-setting"].clone())
    } else {
        None
    };
    let entry = crate::integrations::managed_mcp_entry(paths, options, current.as_ref(), false)?;
    Ok(json!({"mcpServers":{"oh-my-setting":entry}}))
}

fn write_agy_plugin(
    paths: &Paths,
    tx: &mut Transaction,
    source: &std::path::Path,
    target: &std::path::Path,
    mcp: &serde_json::Value,
) -> Result<()> {
    let mut plugin = r::value(&source.join("plugin.json"))?;
    if plugin["name"] != "oh-my-setting" {
        return r::fail("invalid Antigravity plugin template");
    }
    plugin["version"] = json!(
        String::from_utf8(r::read(&paths.source.join("VERSION"), 4096)?)
            .map_err(|_| r::Error(2, "invalid plugin VERSION".into()))?
            .trim()
    );
    let explicit = std::env::var_os("OH_MY_SETTING_AGY_HOOKS").is_some_and(|v| v == "1");
    let certified = crate::agy_surface::verified(paths)? || explicit;
    let tool_certified = crate::agy_surface::tool_hooks_verified(paths)? || explicit;
    let mut entry = json!({
        "PreInvocation":[{"type":"command","command":format!("{} preinvocation", crate::integrations::native_command(paths, "agy-hook")?),"timeout":5}],
        "Stop":[{"type":"command","command":format!("{} stop", crate::integrations::native_command(paths, "agy-hook")?),"timeout":5}]
    });
    if tool_certified {
        let obj = entry.as_object_mut().expect("hook map");
        obj.insert(
            "PreToolUse".into(),
            json!([{"matcher":"invoke_subagent|swarm|run_workflow|manage_subagents","hooks":[{"type":"command","command":format!("{} pretooluse", crate::integrations::native_command(paths, "agy-hook")?),"timeout":5}]}]),
        );
        obj.insert(
            "PostToolUse".into(),
            json!([{"matcher":"invoke_subagent|swarm|run_workflow|manage_subagents","hooks":[{"type":"command","command":format!("{} posttooluse", crate::integrations::native_command(paths, "agy-hook")?),"timeout":5}]}]),
        );
    }
    let hooks = json!({"oh-my-setting": entry});
    let plugin_raw = r::bytes(&plugin)?;
    let mcp_raw = r::bytes(mcp)?;
    let hooks_raw = r::bytes(&hooks)?;
    let mut files = vec![
        ("plugin.json", plugin_raw.as_slice(), 0o600),
        ("mcp_config.json", mcp_raw.as_slice(), 0o600),
    ];
    if certified {
        files.push(("hooks.json", hooks_raw.as_slice(), 0o600));
    }
    tx.files(target, &files)?;
    let row = json!({"schema":1,"kind":"directory","source":source,"sha256":transaction::identity(target)?});
    tx.write(&crate::install::marker(target), &r::bytes(&row)?, 0o600)?;
    if crate::integrations::hud_enabled("OH_MY_SETTING_AGY_HUD") {
        agy_statusline(paths, tx, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture(name: &str) -> (Dir, Paths, PathBuf) {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-agy-hud-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let settings = root.join("home/.gemini/antigravity-cli/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
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
        (Dir(root), paths, settings)
    }
    fn converge(paths: &Paths, remove: bool) {
        let mut tx = Transaction::begin(paths).unwrap();
        agy_statusline(paths, &mut tx, remove).unwrap();
        tx.commit().unwrap();
    }
    fn read(file: &PathBuf) -> Value {
        serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
    }

    #[test]
    fn antigravity_mcp_profiles_reapply_with_owned_directory_identity() {
        let (_dir, paths, _) = fixture("mcp-profiles");
        let source = paths.source.join("plugins/antigravity");
        let target = paths.home.join(".gemini/config/plugins/oh-my-setting");
        let plugins_dir = target.parent().unwrap();
        let file = target.join("mcp_config.json");
        let marker = crate::install::marker(&target);
        let assert_no_plugin_scratch = || {
            if !plugins_dir.is_dir() {
                return;
            }
            for entry in fs::read_dir(plugins_dir).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    !name.starts_with(".oms-stage-")
                        && !name.starts_with(".oms-undo-")
                        && !name.starts_with(".oms-probe-"),
                    "provider-scanned Antigravity plugins dir exposed {name}"
                );
            }
        };
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            br#"{"name":"oh-my-setting","version":"template","description":"fixture"}"#,
        )
        .unwrap();
        fs::write(paths.source.join("VERSION"), b"1.0.0\n").unwrap();
        let options = |args: &[&str]| {
            crate::cli::parse(
                "install-agy-plugin",
                &args
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let bare = options(&[]);
        let full = options(&["--mcp-tool-profile", "full"]);
        let converge = |selected: &Options| -> Result<()> {
            let mcp = agy_mcp(&paths, selected, &source, &target)?;
            let mut tx = Transaction::begin(&paths)?;
            // Production writer after its independent payload validation gate.
            write_agy_plugin(&paths, &mut tx, &source, &target, &mcp)?;
            assert_no_plugin_scratch();
            let res = tx.commit();
            assert_no_plugin_scratch();
            res
        };
        converge(&bare).unwrap();
        assert!(
            read(&file)["mcpServers"]["oh-my-setting"]
                .get("env")
                .is_none()
        );
        // Uncommitted replace and update-snapshot rollback never expose stage/undo in plugins_dir.
        let bare_saved = (fs::read(&file).unwrap(), fs::read(&marker).unwrap());
        {
            let mcp = agy_mcp(&paths, &full, &source, &target).unwrap();
            let mut tx = Transaction::begin(&paths).unwrap();
            write_agy_plugin(&paths, &mut tx, &source, &target, &mcp).unwrap();
            assert_no_plugin_scratch();
            assert_eq!(
                read(&file)["mcpServers"]["oh-my-setting"]["env"]["OMS_MCP_TOOL_PROFILE"],
                "full"
            );
        }
        assert_no_plugin_scratch();
        assert_eq!(
            (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
            bare_saved
        );
        {
            let mut tx = Transaction::begin_update(&paths).unwrap();
            let targets = [target.clone(), marker.clone()];
            let start = tx.prepare_external(&targets).unwrap();
            assert_no_plugin_scratch();
            tx.finish_external(start).unwrap();
        }
        assert_no_plugin_scratch();

        // Crash recovery from new out-of-dir undo and from a legacy sibling undo in plugins_dir.
        let old_dir_id = transaction::identity(&target).unwrap();
        let old_marker_id = transaction::identity(&marker).unwrap();
        let journal_file = paths.config.join("native-lifecycle/transaction.json");
        {
            let mcp = agy_mcp(&paths, &full, &source, &target).unwrap();
            let mut tx = Transaction::begin(&paths).unwrap();
            write_agy_plugin(&paths, &mut tx, &source, &target, &mcp).unwrap();
            assert_no_plugin_scratch();
            let crash_journal = fs::read(&journal_file).unwrap();
            tx.simulate_crash();
            assert!(journal_file.is_file());
            assert_eq!(fs::read(&journal_file).unwrap(), crash_journal);
        }
        Transaction::begin(&paths).unwrap().commit().unwrap();
        assert_no_plugin_scratch();
        assert_eq!(
            (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
            bare_saved
        );
        converge(&full).unwrap();
        let new_dir_id = transaction::identity(&target).unwrap();
        let legacy_undo = plugins_dir.join(".oms-undo-legacy.1");
        let legacy_staged = plugins_dir.join(".oms-stage-legacy.1");
        fs::create_dir_all(&legacy_undo).unwrap();
        fs::write(legacy_undo.join("mcp_config.json"), &bare_saved.0).unwrap();
        // Match the bare plugin.json version ("1.0.0") so legacy undo matches old_dir_id.
        let mut bare_plugin = read(&source.join("plugin.json"));
        bare_plugin["version"] = json!("1.0.0");
        fs::write(
            legacy_undo.join("plugin.json"),
            r::bytes(&bare_plugin).unwrap(),
        )
        .unwrap();
        assert_eq!(transaction::identity(&legacy_undo).unwrap(), old_dir_id);
        let legacy_journal = json!({
            "schema": 1,
            "committed": false,
            "changes": [
                {
                    "target": fs::canonicalize(&target).unwrap(),
                    "staged": fs::canonicalize(plugins_dir).unwrap().join(".oms-stage-legacy.1"),
                    "undo": fs::canonicalize(&legacy_undo).unwrap(),
                    "old": old_dir_id,
                    "new": new_dir_id,
                    "external": false,
                    "parent_identity": null
                },
                {
                    "target": fs::canonicalize(&marker).unwrap(),
                    "staged": fs::canonicalize(plugins_dir).unwrap().join(".oms-stage-legacy.2"),
                    "undo": fs::canonicalize(plugins_dir).unwrap().join(".oms-undo-legacy.2"),
                    "old": old_marker_id,
                    "new": transaction::identity(&marker).unwrap(),
                    "external": false,
                    "parent_identity": null
                }
            ],
            "effects": []
        });
        fs::write(plugins_dir.join(".oms-undo-legacy.2"), &bare_saved.1).unwrap();
        fs::write(&journal_file, serde_json::to_vec(&legacy_journal).unwrap()).unwrap();
        Transaction::begin(&paths).unwrap().commit().unwrap();
        assert!(!legacy_undo.exists());
        assert!(!legacy_staged.exists());
        assert_no_plugin_scratch();
        assert_eq!(
            (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
            bare_saved
        );
        // Legacy `committed: true` journal with sibling undos in `plugins_dir` cleans up scratch.
        converge(&full).unwrap();
        let full_saved = (fs::read(&file).unwrap(), fs::read(&marker).unwrap());
        let committed_undo_dir = plugins_dir.join(".oms-undo-legacy.3");
        fs::create_dir_all(&committed_undo_dir).unwrap();
        fs::write(committed_undo_dir.join("mcp_config.json"), &bare_saved.0).unwrap();
        fs::write(
            committed_undo_dir.join("plugin.json"),
            r::bytes(&bare_plugin).unwrap(),
        )
        .unwrap();
        let committed_undo_marker = plugins_dir.join(".oms-undo-legacy.4");
        fs::write(&committed_undo_marker, &bare_saved.1).unwrap();
        let committed_legacy_journal = json!({
            "schema": 1,
            "committed": true,
            "changes": [
                {
                    "target": fs::canonicalize(&target).unwrap(),
                    "staged": fs::canonicalize(plugins_dir).unwrap().join(".oms-stage-legacy.3"),
                    "undo": fs::canonicalize(&committed_undo_dir).unwrap(),
                    "old": old_dir_id,
                    "new": new_dir_id,
                    "external": false,
                    "parent_identity": null
                },
                {
                    "target": fs::canonicalize(&marker).unwrap(),
                    "staged": fs::canonicalize(plugins_dir).unwrap().join(".oms-stage-legacy.4"),
                    "undo": fs::canonicalize(&committed_undo_marker).unwrap(),
                    "old": old_marker_id,
                    "new": transaction::identity(&marker).unwrap(),
                    "external": false,
                    "parent_identity": null
                }
            ],
            "effects": []
        });
        fs::write(
            &journal_file,
            serde_json::to_vec(&committed_legacy_journal).unwrap(),
        )
        .unwrap();
        Transaction::begin(&paths).unwrap().commit().unwrap();
        assert!(!committed_undo_dir.exists());
        assert!(!committed_undo_marker.exists());
        assert_no_plugin_scratch();
        assert_eq!(
            (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
            full_saved
        );
        converge(&bare).unwrap();
        for profile in ["full", "core"] {
            converge(&options(&["--mcp-tool-profile", profile])).unwrap();
            assert_eq!(
                read(&file)["mcpServers"]["oh-my-setting"]["env"]["OMS_MCP_TOOL_PROFILE"],
                profile
            );
            assert!(crate::install::owned(&source, &target).unwrap());
            let saved = (fs::read(&file).unwrap(), fs::read(&marker).unwrap());
            converge(&bare).unwrap();
            assert_eq!(
                (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
                saved
            );
        }
        // Even a valid profile edit cannot bypass the native directory digest.
        let saved = (fs::read(&file).unwrap(), fs::read(&marker).unwrap());
        let mut changed = read(&file);
        changed["mcpServers"]["oh-my-setting"]["env"]["OMS_MCP_TOOL_PROFILE"] = json!("full");
        fs::write(&file, r::bytes(&changed).unwrap()).unwrap();
        let edited = fs::read(&file).unwrap();
        assert!(converge(&full).is_err());
        assert_eq!(fs::read(&file).unwrap(), edited);
        assert_eq!(fs::read(&marker).unwrap(), saved.1);

        // A known directory identity cannot authorize dropping unknown env or
        // adopting another executable within its MCP registration.
        for replacement in [
            json!({"env":{"OMS_MCP_TOOL_PROFILE":"unknown"}}),
            json!({"env":{"OMS_MCP_TOOL_PROFILE":"full","USER_CUSTOM":"1"}}),
            json!({"command":"other-mcp","env":{"OMS_MCP_TOOL_PROFILE":"full"}}),
        ] {
            let mut changed = r::parse(&saved.0).unwrap();
            changed["mcpServers"]["oh-my-setting"]
                .as_object_mut()
                .unwrap()
                .extend(replacement.as_object().unwrap().clone());
            fs::write(&file, r::bytes(&changed).unwrap()).unwrap();
            let mut recorded = r::parse(&saved.1).unwrap();
            recorded["sha256"] = json!(transaction::identity(&target).unwrap());
            fs::write(&marker, r::bytes(&recorded).unwrap()).unwrap();
            assert!(crate::install::owned(&source, &target).unwrap());
            let before = (fs::read(&file).unwrap(), fs::read(&marker).unwrap());
            assert!(converge(&full).is_err());
            assert_eq!(
                (fs::read(&file).unwrap(), fs::read(&marker).unwrap()),
                before
            );
        }
        fs::write(&file, &saved.0).unwrap();
        fs::write(&marker, &saved.1).unwrap();
        converge(&full).unwrap();
        let mut tx = Transaction::begin(&paths).unwrap();
        apply(
            &paths,
            &options(&["--remove"]),
            &mut tx,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_no_plugin_scratch();
        tx.commit().unwrap();
        assert_no_plugin_scratch();
        assert!(!target.exists());
        assert!(!marker.exists());

        // Exact legacy plugin directories can retain the known profile while
        // moving to the native command and current ownership sidecar.
        fs::create_dir_all(&target).unwrap();
        fs::copy(source.join("plugin.json"), target.join("plugin.json")).unwrap();
        fs::write(
            &file,
            r::bytes(&json!({"mcpServers":{"oh-my-setting":{
                "command":"python3","args":[paths.source.join("scripts/oms-mcp-server.py")],
                "env":{"OMS_MCP_TOOL_PROFILE":"full"}
            }}}))
            .unwrap(),
        )
        .unwrap();
        converge(&bare).unwrap();
        assert_eq!(
            read(&file)["mcpServers"]["oh-my-setting"]["env"]["OMS_MCP_TOOL_PROFILE"],
            "full"
        );
        assert_eq!(
            read(&file)["mcpServers"]["oh-my-setting"]["args"],
            json!(["mcp-server"])
        );
        assert!(crate::install::owned(&source, &target).unwrap());
    }

    #[test]
    fn antigravity_status_line_installs_idempotently_and_removes_only_its_own() {
        let (_dir, paths, file) = fixture("owned");
        fs::write(&file, r#"{"theme":"x"}"#).unwrap();
        converge(&paths, false);
        let line = read(&file)["statusLine"].clone();
        assert!(
            line["command"]
                .as_str()
                .unwrap()
                .ends_with(" agy-statusline")
        );
        assert_eq!(
            (
                &line["type"],
                &line["enabled"],
                &line["padding"],
                &line["stack_with_default"]
            ),
            (&json!("command"), &json!(true), &json!(0), &json!(false))
        );
        let installed = fs::read(&file).unwrap();
        converge(&paths, false);
        assert_eq!(fs::read(&file).unwrap(), installed);
        converge(&paths, true);
        assert_eq!(read(&file), json!({"theme":"x"}));
        assert!(
            file.with_file_name("settings.json.oh-my-setting-backup")
                .exists()
        );
        use std::os::unix::fs::PermissionsExt;
        let ext = paths
            .home
            .join(".codex/plugins/cache/oh-my-setting-local/oh-my-setting/0.7.0");
        fs::create_dir_all(&ext).unwrap();
        fs::set_permissions(&ext, fs::Permissions::from_mode(0o775)).unwrap();
        let hook = ext.join("hooks.json");
        fs::write(&hook, b"{}").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o666)).unwrap();
        let mut tx = Transaction::begin(&paths).unwrap();
        let start = tx.prepare_external(std::slice::from_ref(&ext)).unwrap();
        tx.finish_external(start).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn antigravity_status_line_keeps_similarly_named_user_commands() {
        let (_dir, paths, file) = fixture("similar");
        let custom = r#"{"statusLine":{"type":"command","command":"/x/agy-statusline-custom"}}"#;
        fs::write(&file, custom).unwrap();
        converge(&paths, false);
        converge(&paths, true);
        assert_eq!(fs::read_to_string(&file).unwrap(), custom);
    }

    #[test]
    fn antigravity_status_line_keeps_user_entries_and_unreadable_files() {
        let (_dir, paths, file) = fixture("user");
        let custom = r#"{"statusLine":{"type":"command","command":"my-hud"}}"#;
        fs::write(&file, custom).unwrap();
        converge(&paths, false);
        converge(&paths, true);
        assert_eq!(fs::read_to_string(&file).unwrap(), custom);
        fs::write(&file, "// not json").unwrap();
        converge(&paths, false);
        assert_eq!(fs::read_to_string(&file).unwrap(), "// not json");
        fs::remove_file(&file).unwrap();
        converge(&paths, true);
        assert!(!file.exists());
        converge(&paths, false);
        assert!(read(&file)["statusLine"]["command"].is_string());
    }
}
