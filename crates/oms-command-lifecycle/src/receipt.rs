use crate::runtime::{self as r, Result};
use serde_json::{Value, json};
use std::{
    env,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone)]
pub(crate) struct Paths {
    pub home: PathBuf,
    pub source: PathBuf,
    pub receipt: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
    pub cwd: PathBuf,
}
impl Paths {
    pub fn new(cwd: &Path, source: &Path) -> Result<Self> {
        let cwd = r::physical(cwd)?;
        let source = r::physical(source)?;
        let home = r::physical(
            &env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| r::Error(2, "HOME is required".into()))?,
        )?;
        let base = |key: &str, default: PathBuf| -> Result<PathBuf> {
            r::physical(&env::var_os(key).map(PathBuf::from).unwrap_or(default))
        };
        let config = base("XDG_CONFIG_HOME", home.join(".config"))?.join("oh-my-setting");
        let state = base("XDG_STATE_HOME", home.join(".local/state"))?.join("oh-my-setting");
        let data = base("XDG_DATA_HOME", home.join(".local/share"))?.join("oh-my-setting");
        let receipt = r::leaf(
            &env::var_os("OMS_INSTALL_RECEIPT")
                .map(PathBuf::from)
                .unwrap_or_else(|| config.join("install.json")),
        )?;
        Ok(Self {
            home,
            source,
            receipt,
            config,
            state,
            data,
            cwd,
        })
    }
    pub fn claude_settings(&self) -> Result<PathBuf> {
        r::leaf(
            &env::var_os("OMS_CLAUDE_SETTINGS")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".claude/settings.json")),
        )
    }
    pub fn codex_home(&self) -> Result<PathBuf> {
        r::physical(
            &env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".codex")),
        )
    }
    pub fn pi_home(&self) -> Result<PathBuf> {
        r::physical(
            &env::var_os("PI_CODING_AGENT_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".pi/agent")),
        )
    }
    pub fn codex_config(&self) -> Result<PathBuf> {
        r::leaf(
            &env::var_os("OMS_CODEX_CONFIG")
                .map(PathBuf::from)
                .unwrap_or(self.codex_home()?.join("config.toml")),
        )
    }
    pub fn load(&self) -> Result<Option<Value>> {
        if !r::exists(&self.receipt) {
            return Ok(None);
        }
        let row = r::value(&self.receipt)?;
        validate(&row)?;
        Ok(Some(row))
    }
    pub fn require_owner(&self) -> Result<Option<Value>> {
        let row = self.load()?;
        if let Some(row) = &row
            && owner(row)? != self.source
        {
            return r::fail("this checkout does not own the canonical installation");
        }
        Ok(row)
    }
}
pub(crate) fn owner(row: &Value) -> Result<PathBuf> {
    let source = row
        .get("source_root")
        .and_then(Value::as_str)
        .ok_or_else(|| r::Error(2, "invalid install owner".into()))?;
    r::physical(Path::new(source))
}
pub(crate) fn validate(v: &Value) -> Result<()> {
    if !matches!(v.get("schema").and_then(Value::as_u64), Some(1 | 2)) {
        return r::fail("invalid install receipt schema");
    }
    owner(v)?;
    for k in ["commit", "channel", "version", "installed_at"] {
        if v.get(k).and_then(Value::as_str).is_none_or(str::is_empty) {
            return r::fail(format!("invalid install receipt field: {k}"));
        }
    }
    for k in ["name", "version", "sha256"] {
        if v.get("plugin")
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return r::fail("invalid install plugin metadata");
        }
    }
    if v["schema"] == 2 {
        if !matches!(v["profile"].as_str(), Some("minimal" | "full" | "custom"))
            || v["ref"].as_str().is_none_or(str::is_empty)
            || v["previous_commit"].as_str().is_none()
        {
            return r::fail("invalid install receipt profile");
        }
        if !v["link_mode"].is_null() && !matches!(v["link_mode"].as_str(), Some("copy" | "symlink"))
        {
            return r::fail("invalid install link mode");
        }
        for k in [
            "tools",
            "claude_hooks",
            "codex_plugin",
            "auto_update",
            "machine_snapshot",
            "slurm_snapshot",
        ] {
            if !v["components"][k].is_boolean() {
                return r::fail("invalid install component profile");
            }
        }
        if !v["component_modes"].is_null() && !v["component_modes"].is_object() {
            return r::fail("invalid install component modes");
        }
        for k in ["machine_snapshot", "slurm_snapshot"] {
            if let Some(x) = v["component_modes"].get(k)
                && !matches!(x.as_str(), Some("0" | "1" | "auto"))
            {
                return r::fail("invalid snapshot mode");
            }
        }
        if let Some(x) = v["component_modes"].get("auto_update")
            && !matches!(x.as_str(), Some("check" | "apply"))
        {
            return r::fail("invalid auto-update mode");
        }
        if v["managed_targets"]
            .as_array()
            .is_none_or(|a| a.iter().any(|x| x.as_str().is_none_or(str::is_empty)))
        {
            return r::fail("invalid managed target inventory");
        }
    }
    Ok(())
}
pub(crate) fn build(paths: &Paths, old: Option<&Value>, flag: &AtomicBool) -> Result<Value> {
    let version = String::from_utf8(r::read(&paths.source.join("VERSION"), 4096)?)
        .map_err(|_| r::Error(2, "invalid VERSION".into()))?
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_owned();
    let commit =
        r::git(&paths.source, &["rev-parse", "HEAD"], flag).unwrap_or_else(|_| "unknown".into());
    let channel = r::git(
        &paths.source,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        flag,
    )
    .unwrap_or_else(|_| "detached".into());
    let dirty = r::git(
        &paths.source,
        &["status", "--porcelain", "--untracked-files=normal"],
        flag,
    )
    .is_ok_and(|s| !s.is_empty());
    let mut row = old.cloned().unwrap_or_else(|| json!({}));
    for (k, v) in [
        ("schema", json!(2)),
        ("source_root", json!(paths.source)),
        ("commit", json!(commit)),
        ("channel", json!(channel)),
        ("version", json!(version)),
        ("dirty", json!(dirty)),
        ("installed_at", json!(r::now())),
        ("claude_settings_path", json!(paths.claude_settings()?)),
    ] {
        row[k] = v;
    }
    row["profile"] = json!(env::var("OH_MY_SETTING_PROFILE").unwrap_or_else(|_| {
        old.and_then(|v| v["profile"].as_str())
            .unwrap_or("custom")
            .into()
    }));
    row["ref"] = json!(env::var("OH_MY_SETTING_REF").unwrap_or_else(|_| {
        old.and_then(|v| v["ref"].as_str())
            .unwrap_or("edge")
            .into()
    }));
    row["previous_commit"] = json!(
        old.and_then(|v| v["previous_commit"].as_str())
            .unwrap_or("")
    );
    row["link_mode"] = json!(link_mode()?);
    if !row["components"].is_object() {
        row["components"] = json!({});
    }
    for (key, var, default) in [
        ("tools", "OH_MY_SETTING_INSTALL_TOOLS", false),
        ("claude_hooks", "OH_MY_SETTING_CLAUDE_HOOKS", true),
        ("codex_plugin", "OH_MY_SETTING_CODEX_PLUGIN", false),
        ("mcp", "OH_MY_SETTING_MCP", true),
        ("agy_plugin", "OH_MY_SETTING_AGY_PLUGIN", false),
        ("auto_update", "OH_MY_SETTING_AUTO_UPDATE", false),
        ("machine_snapshot", "OH_MY_SETTING_GENERATE_MACHINE", false),
        ("slurm_snapshot", "OH_MY_SETTING_GENERATE_SLURM", false),
    ] {
        // Antigravity auto-enables when its CLI directory exists, unless the
        // install explicitly opted out (stored mode "0") or pinned it on.
        let agy_auto = key == "agy_plugin"
            && !matches!(
                old.and_then(|v| v["component_modes"]["agy_plugin"].as_str()),
                Some("0" | "1")
            );
        let enabled = match env::var(var).ok().as_deref() {
            Some("0") => false,
            Some("1") => true,
            Some("auto") if agy_auto => paths.home.join(".gemini/antigravity-cli").exists(),
            Some("auto")
                if matches!(key, "machine_snapshot" | "slurm_snapshot" | "codex_plugin") =>
            {
                key == "machine_snapshot"
            }
            Some("auto") if key == "agy_plugin" => false,
            Some(_) => return r::fail(format!("invalid component setting: {var}")),
            None if agy_auto => {
                paths.home.join(".gemini/antigravity-cli").exists()
                    || row["components"][key].as_bool().unwrap_or(default)
            }
            None => row["components"][key].as_bool().unwrap_or(default),
        };
        row["components"][key] = json!(enabled);
    }
    if !row["component_modes"].is_object() {
        row["component_modes"] = json!({});
    }
    for (k, var, default) in [
        ("auto_update", "OH_MY_SETTING_AUTO_UPDATE_MODE", "apply"),
        ("machine_snapshot", "OH_MY_SETTING_GENERATE_MACHINE", "0"),
        ("slurm_snapshot", "OH_MY_SETTING_GENERATE_SLURM", "0"),
        ("agy_plugin", "OH_MY_SETTING_AGY_PLUGIN", "auto"),
    ] {
        row["component_modes"][k] = json!(
            env::var(var)
                .unwrap_or_else(|_| row["component_modes"][k].as_str().unwrap_or(default).into())
        );
    }
    row["managed_targets"] = json!([
        ".codex/AGENTS.md",
        ".claude/CLAUDE.md",
        ".gemini/AGENTS.md",
        ".codex/skills",
        ".claude/skills",
        ".gemini/config/skills",
        ".gemini/antigravity/skills",
        ".local/bin/oms"
    ]);
    let plugin = paths.source.join("plugins/oh-my-setting");
    let pv = r::value(&plugin.join(".codex-plugin/plugin.json"))
        .ok()
        .and_then(|v| v["version"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    row["plugin"] = json!({"name":"oh-my-setting", "version":pv, "sha256": crate::fingerprint::tree(&plugin, false).unwrap_or_else(|_| "unknown".into())});
    validate(&row)?;
    Ok(row)
}
pub(crate) fn link_mode() -> Result<String> {
    match env::var("OH_MY_SETTING_LINK_MODE")
        .unwrap_or_else(|_| "auto".into())
        .as_str()
    {
        "auto" | "symlink" => Ok("symlink".into()),
        "copy" => Ok("copy".into()),
        _ => r::fail("OH_MY_SETTING_LINK_MODE must be auto, symlink, or copy"),
    }
}
