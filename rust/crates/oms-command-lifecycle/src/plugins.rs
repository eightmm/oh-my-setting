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
fn inventory(paths: &Paths, flag: &AtomicBool) -> Result<(bool, Option<PathBuf>)> {
    let out = codex(paths, &["plugin", "list", "--json"], flag)?;
    if out.0 != 0 {
        return r::fail("Codex plugin inventory unavailable; no configuration changed");
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
    Ok((installed, root))
}
fn cache(paths: &Paths) -> Result<PathBuf> {
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
    let (installed, root) = inventory(paths, flag)?;
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
                && root.join("scripts/install-codex-plugin.sh").is_file())
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
        crate::settings::codex(tx, &paths.codex_config()?, true)?;
    }
    if r::find("codex", &paths.cwd).is_none() {
        return Ok(());
    }
    let (installed, root) = inventory(paths, flag)?;
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
            if root.is_some() {
                commands.push(vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "remove".into(),
                    MARKETPLACE.into(),
                ]);
            }
        } else {
            let destination = cache(paths)?;
            let stale = !r::exists(&destination)
                || crate::fingerprint::tree(&destination, false)?
                    != crate::fingerprint::tree(
                        &paths.source.join("plugins/oh-my-setting"),
                        false,
                    )?;
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
    let (present, market) = inventory(paths, flag)?;
    if remove {
        if present || market.is_some() {
            return r::fail("Codex CLI did not remove its plugin and marketplace");
        }
        return Ok(());
    }
    if !present || market.is_none() {
        return r::fail("Codex CLI did not register its plugin and marketplace");
    }
    let cache = cache(paths)?;
    owned_cache(paths, &cache, false)?;
    let source = paths.source.join("plugins/oh-my-setting");
    let expected = crate::fingerprint::tree(&source, false)?;
    if crate::fingerprint::tree(&cache, false)? != expected {
        tx.publish_external(&cache, &source)?;
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
    crate::settings::codex(tx, &paths.codex_config()?, false)
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
    if (r::exists(&target) || r::exists(&marker)) && !crate::install::owned(&source, &target)? {
        return r::fail("unmanaged or modified Antigravity plugin; takeover refused");
    }
    if remove {
        tx.remove(&target)?;
        if r::exists(&marker) {
            tx.remove(&marker)?;
        }
        return Ok(());
    }
    crate::payload::verify(paths, flag)?.recheck()?;
    let mut plugin = r::value(&source.join("plugin.json"))?;
    if plugin["name"] != "oh-my-setting" {
        return r::fail("invalid Antigravity plugin template");
    }
    plugin["version"] = json!(
        String::from_utf8(r::read(&paths.source.join("VERSION"), 4096)?)
            .map_err(|_| r::Error(2, "invalid plugin VERSION".into()))?
            .trim()
    );
    let mcp = json!({"mcpServers":{"oh-my-setting":{"command":paths.home.join(".local/bin/oms"),"args":["mcp-server"]}}});
    let certified = crate::agy_surface::verified(paths)?;
    let hooks = json!({"oh-my-setting":{
        "PreInvocation":[{"type":"command","command":"bash ./agy-hook.sh preinvocation","timeout":5}],
        "Stop":[{"type":"command","command":"bash ./agy-hook.sh stop","timeout":5}]
    }});
    let launcher = format!(
        "#!/bin/sh\n# oh-my-setting native Antigravity lifecycle adapter\nexec {} agy-hook \"$@\"\n",
        r::shell(paths.home.join(".local/bin/oms").as_os_str())?
    );
    let plugin_raw = r::bytes(&plugin)?;
    let mcp_raw = r::bytes(&mcp)?;
    let hooks_raw = r::bytes(&hooks)?;
    let mut files = vec![
        ("plugin.json", plugin_raw.as_slice(), 0o600),
        ("mcp_config.json", mcp_raw.as_slice(), 0o600),
    ];
    if certified {
        files.push(("hooks.json", hooks_raw.as_slice(), 0o600));
        files.push(("agy-hook.sh", launcher.as_bytes(), 0o755));
    }
    tx.files(&target, &files)?;
    let row = json!({"schema":1,"kind":"directory","source":source,"sha256":transaction::identity(&target)?});
    tx.write(&marker, &r::bytes(&row)?, 0o600)
}
