use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::{self, Transaction},
};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

pub(crate) fn preflight(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<()> {
    if options.has("--probe-surfaces") {
        return r::fail(
            "Antigravity surface certification requires the native isolated provider-probe service; no model call made",
        );
    }
    if options.tool == "install-agy-plugin" {
        return Ok(());
    }
    let Some(binary) = r::find("codex", &paths.cwd) else {
        if options.has("--remove") {
            let file = paths.codex_config()?;
            if r::exists(&file) {
                let config = crate::config::read_toml(&file)?;
                if config["plugins"]
                    .get("oh-my-setting@oh-my-setting-local")
                    .is_some()
                    || config["marketplaces"].get("oh-my-setting-local").is_some()
                {
                    return r::fail(
                        "Codex plugin configuration is present; its CLI is required for removal",
                    );
                }
            }
            return Ok(());
        }
        return r::fail("Codex is unavailable for plugin inventory");
    };
    let output = r::command(
        binary,
        &["plugin".into(), "list".into(), "--json".into()],
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(120),
    )?;
    if output.0 != 0 {
        return r::fail("Codex plugin inventory unavailable; no configuration changed");
    }
    let value = r::parse(&output.1)?;
    let installed = value["installed"]
        .as_array()
        .ok_or_else(|| r::Error(2, "invalid Codex plugin inventory".into()))?
        .iter()
        .any(|r| r["pluginId"] == "oh-my-setting@oh-my-setting-local" && r["installed"] == true);
    if options.has("--remove") && installed {
        return r::fail(
            "Codex plugin removal requires its provider-owned transaction service; configuration retained",
        );
    }
    if !options.has("--remove") && !installed {
        return r::fail(
            "Codex plugin registration requires its provider-owned transaction service; configuration retained",
        );
    }
    Ok(())
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
        if !remove {
            let source = paths.source.join("plugins/oh-my-setting");
            let version = r::value(&source.join(".codex-plugin/plugin.json"))?["version"]
                .as_str()
                .filter(|s| r::safe_name(s) && !matches!(*s, "." | ".."))
                .ok_or_else(|| r::Error(2, "invalid Codex plugin version".into()))?
                .to_owned();
            let cache = paths
                .codex_home()?
                .join("plugins/cache/oh-my-setting-local/oh-my-setting")
                .join(version);
            let base = paths.codex_home()?;
            let mut ancestor = cache.as_path();
            while ancestor != base {
                let metadata = std::fs::symlink_metadata(ancestor)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return r::fail("Codex plugin cache must use physical directories");
                }
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| r::Error(2, "Codex plugin cache escaped its home".into()))?;
            }
            let expected = crate::fingerprint::tree(&source, false)?;
            if crate::fingerprint::tree(&cache, false)? != expected {
                let old_root = String::from_utf8(r::read(
                    &cache.join(".oh-my-setting-source-root"),
                    r::LIMIT,
                )?)
                .map_err(|_| r::Error(2, "invalid Codex cache ownership".into()))?;
                let recorded =
                    String::from_utf8(r::read(&cache.join(".oh-my-setting-source-sha256"), 128)?)
                        .map_err(|_| r::Error(2, "invalid Codex cache digest".into()))?;
                let root = r::physical(&PathBuf::from(old_root.trim()))?;
                let owner = paths
                    .load()?
                    .as_ref()
                    .map(crate::receipt::owner)
                    .transpose()?;
                if root != paths.source && owner.as_ref() != Some(&root)
                    || crate::fingerprint::tree(&cache, false)? != recorded.trim()
                {
                    return r::fail("modified or foreign Codex plugin cache; refresh refused");
                }
                tx.copy(&cache, &source)?;
            }
            tx.write(
                &cache.join(".oh-my-setting-source-root"),
                format!(
                    "{}\n",
                    paths.source.to_str().ok_or_else(|| r::Error(
                        2,
                        "plugin provenance requires a UTF-8 source path".into()
                    ))?
                )
                .as_bytes(),
                0o600,
            )?;
            tx.write(
                &cache.join(".oh-my-setting-source-sha256"),
                format!("{expected}\n").as_bytes(),
                0o600,
            )?;
        }
        return crate::settings::codex(tx, &paths.codex_config()?, remove);
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
    // A cache assertion cannot certify executable hook delivery or enforcement.
    // Replacing the complete owned directory retracts any stale hook adapter.
    tx.files(
        &target,
        &[
            ("plugin.json", &r::bytes(&plugin)?, 0o600),
            ("mcp_config.json", &r::bytes(&mcp)?, 0o600),
        ],
    )?;
    let row = json!({"schema":1,"kind":"directory","source":source,"sha256":transaction::identity(&target)?});
    tx.write(&marker, &r::bytes(&row)?, 0o600)
}
