use crate::{Error, Output, Result, cli::Options, json, parent_only, render, require};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{
    INVALID_MAP_MSG, MAX_BYTES, MAX_ENTRIES, TargetKind, TempMapFile, check_alias,
    check_destination_unchanged, lock_map_file, open_parent_dir, physical_parent_and_leaf,
    read_map_at, read_private_sidecar_at, resolve_writer_path, same_stat, sync_dir, valid_alias,
    validate_checkout, validate_map_stat, validate_private_sidecar_stat, write_atomic,
};

pub(crate) const PANELS_FILE_NAME: &str = "desktop-project-panels.json";
const PANEL_ENTRY_NAME: &str = "oms-panel";
const LIFECYCLE_OWNED_KEY: &str = "oh-my-setting";
const LIFECYCLE_CODEX_MARKETPLACE: &str = "oh-my-setting-local";
const LIFECYCLE_CODEX_PANEL_PLUGIN: &str = "oms";
const CODEX_MARKETPLACE_NAME: &str = "oms-host-panels";
const CODEX_MARKETPLACE_DISPLAY_NAME: &str = "OMS project panels";
const CODEX_MARKETPLACE_MARKER: &str = ".oh-my-setting-codex-panels.json";
const OMS_PACKAGE_VERSION: &str = include_str!("../../../VERSION").trim_ascii();
const REPO_OMS_PLUGIN_JSON: &str = include_str!("../../../plugins/oms/plugin.json");
const REPO_OMS_MCP_JSON: &str = include_str!("../../../plugins/oms/mcp.json");
const VENDOR_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const MCP_MISSING_STATUS: i32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PanelApp {
    Claude,
    Codex,
}

impl PanelApp {
    pub(crate) const SUPPORTED: &'static str = "claude, codex";

    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(Error::new(format!(
                "unsupported panel app '{raw}' (supported: {})",
                Self::SUPPORTED
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    const fn expected_scope(self) -> &'static str {
        match self {
            Self::Claude => "local",
            Self::Codex => "user",
        }
    }

    const fn cli_label(self) -> &'static str {
        match self {
            Self::Claude => "Claude CLI",
            Self::Codex => "Codex CLI",
        }
    }

    const fn bin_env_and_default(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("OMS_CLAUDE_BIN", "claude"),
            Self::Codex => ("OMS_CODEX_BIN", "codex"),
        }
    }

    fn expected_entry_name(self, alias: &str) -> Result<String> {
        match self {
            Self::Claude => Ok(PANEL_ENTRY_NAME.to_owned()),
            Self::Codex => codex_plugin_id(alias),
        }
    }
}

fn codex_plugin_name(alias: &str) -> Result<String> {
    if alias.contains('.') {
        return Err(Error::new(format!(
            "project alias '{alias}' cannot form a valid Codex plugin name 'oms-{alias}': aliases containing '.' are not supported for Codex panels (expected 1 to 60 lowercase ASCII letters or digits separated by single '-')"
        )));
    }
    let valid = (1..=60).contains(&alias.len())
        && alias
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && alias
            .as_bytes()
            .last()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !alias.as_bytes().windows(2).any(|w| w == b"--");
    if !valid {
        return Err(Error::new(format!(
            "project alias '{alias}' cannot form a valid Codex plugin name 'oms-{alias}': expected 1 to 60 lowercase ASCII letters or digits separated by single '-'"
        )));
    }
    let name = format!("oms-{alias}");
    debug_assert_ne!(name, LIFECYCLE_OWNED_KEY);
    debug_assert_ne!(name, LIFECYCLE_CODEX_PANEL_PLUGIN);
    Ok(name)
}

fn codex_plugin_id(alias: &str) -> Result<String> {
    let plugin_name = codex_plugin_name(alias)?;
    Ok(format!("{plugin_name}@{CODEX_MARKETPLACE_NAME}"))
}

struct CodexPluginTemplate {
    plugin_schema: String,
    description: String,
    short_description: String,
    mcp_schema: String,
    panel_resource_version: String,
}

fn codex_plugin_template() -> &'static CodexPluginTemplate {
    static TEMPLATE: OnceLock<CodexPluginTemplate> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let plugin_val: serde_json::Value =
            serde_json::from_str(REPO_OMS_PLUGIN_JSON).expect("compiled plugins/oms/plugin.json");
        let mcp_val: serde_json::Value =
            serde_json::from_str(REPO_OMS_MCP_JSON).expect("compiled plugins/oms/mcp.json");
        CodexPluginTemplate {
            plugin_schema: plugin_val["$schema"]
                .as_str()
                .expect("plugin $schema")
                .to_owned(),
            description: plugin_val["description"]
                .as_str()
                .expect("plugin description")
                .to_owned(),
            short_description:
                plugin_val["extensions"]["com.openai"]["interface"]["shortDescription"]
                    .as_str()
                    .expect("plugin shortDescription")
                    .to_owned(),
            mcp_schema: mcp_val["$schema"].as_str().expect("mcp $schema").to_owned(),
            panel_resource_version:
                mcp_val["mcpServers"]["oms-panel"]["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"]
                    .as_str()
                    .expect("OMS_MCP_PANEL_RESOURCE_VERSION in plugins/oms/mcp.json")
                    .to_owned(),
        }
    })
}

fn codex_panel_resource_version() -> &'static str {
    &codex_plugin_template().panel_resource_version
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PanelRecord {
    pub app: String,
    pub scope: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_sha256: Option<String>,
}

fn valid_sha256_hex(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

pub(super) type PanelMap = BTreeMap<String, Vec<PanelRecord>>;

struct StrictPanelMap(PanelMap);

impl<'de> serde::Deserialize<'de> for StrictPanelMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct PanelVisitor;
        impl<'de> serde::de::Visitor<'de> for PanelVisitor {
            type Value = StrictPanelMap;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object of project aliases and installed panel arrays")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut access: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Vec<PanelRecord>>()? {
                    if map.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate project alias '{key}'"
                        )));
                    }
                    map.insert(key, value);
                }
                Ok(StrictPanelMap(map))
            }
        }
        deserializer.deserialize_map(PanelVisitor)
    }
}

fn normalized_abs_utf8(path: &Path) -> bool {
    let raw = path.as_os_str().as_bytes();
    path.is_absolute()
        && path.to_str().is_some()
        && !raw.contains(&0)
        && (raw == b"/" || !raw.ends_with(b"/"))
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

pub(super) fn validate_panels_stat(stat: &Stat) -> Result<()> {
    validate_private_sidecar_stat(stat, "OMS project panel map", "desktop project panel map")
}

fn parse_panels_json(raw: &str) -> Result<PanelMap> {
    require(
        raw.len() <= MAX_BYTES,
        "Invalid OMS project panel map: exceeds 16 KiB",
    )?;
    let StrictPanelMap(raw_map) = serde_json::from_str(raw).map_err(|_| {
        Error::new(
            "Invalid OMS project panel map: expected an object of project aliases and strict panel records",
        )
    })?;
    require(
        raw_map.len() <= MAX_ENTRIES,
        "Invalid OMS project panel map: exceeds 128 entries",
    )?;
    let mut out = BTreeMap::new();
    for (alias, rows) in raw_map {
        require(
            valid_alias(&alias) && !rows.is_empty() && rows.len() <= 8,
            "Invalid OMS project panel map: expected valid aliases with nonempty panel lists",
        )?;
        let mut seen = BTreeSet::new();
        for row in &rows {
            let app = PanelApp::parse(&row.app).map_err(|_| {
                Error::new("Invalid OMS project panel map: unsupported panel app in record")
            })?;
            let expected_name = app.expected_entry_name(&alias).map_err(|_| {
                Error::new(
                    "Invalid OMS project panel map: alias cannot form a valid panel entry name",
                )
            })?;
            let hashes_valid = match app {
                PanelApp::Claude => row.plugin_sha256.is_none() && row.mcp_sha256.is_none(),
                PanelApp::Codex => {
                    row.plugin_sha256.as_deref().is_some_and(valid_sha256_hex)
                        && row.mcp_sha256.as_deref().is_some_and(valid_sha256_hex)
                }
            };
            require(
                row.scope == app.expected_scope()
                    && row.name == expected_name
                    && row.name != LIFECYCLE_OWNED_KEY
                    && normalized_abs_utf8(&row.path)
                    && hashes_valid
                    && seen.insert(app),
                "Invalid OMS project panel map: invalid or duplicate panel record fields",
            )?;
        }
        out.insert(alias, rows);
    }
    Ok(out)
}

fn encode_panels_json(panels: &PanelMap) -> Result<Vec<u8>> {
    require(
        !panels.is_empty() && panels.len() <= MAX_ENTRIES,
        "desktop project panel map must contain 1 to 128 entries",
    )?;
    let mut bytes = serde_json::to_vec_pretty(panels).map_err(Error::new)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        bytes = serde_json::to_vec(panels).map_err(Error::new)?;
        bytes.push(b'\n');
    }
    require(
        bytes.len() <= MAX_BYTES,
        "desktop project panel map exceeds 16 KiB",
    )?;
    Ok(bytes)
}

pub(super) fn read_panels_at(dir_fd: &OwnedFd) -> Result<Option<(PanelMap, Stat)>> {
    let Some((raw, stat)) = read_private_sidecar_at(
        dir_fd,
        OsStr::new(PANELS_FILE_NAME),
        "OMS project panel map",
        "desktop project panel map",
    )?
    else {
        return Ok(None);
    };
    Ok(Some((parse_panels_json(&raw)?, stat)))
}

pub(super) fn write_or_unlink_panels(
    dir_fd: &OwnedFd,
    before: Option<&Stat>,
    panels: &PanelMap,
) -> Result<()> {
    let name = OsStr::new(PANELS_FILE_NAME);
    if panels.is_empty() {
        check_destination_unchanged(dir_fd, name, before, TargetKind::PanelSidecar)?;
        if before.is_some() {
            fs::unlinkat(dir_fd, name, AtFlags::empty()).map_err(|e| Error {
                code: 1,
                message: format!("cannot remove empty desktop project panel map: {e}"),
                publication_uncertain: true,
            })?;
            sync_dir(dir_fd)?;
        }
        Ok(())
    } else {
        let bytes = encode_panels_json(panels)?;
        write_atomic(dir_fd, name, before, &bytes, TargetKind::PanelSidecar)
    }
}

fn unknown_alias_error(alias: &str) -> Error {
    Error::new(format!(
        "unknown project alias '{alias}'; register it first with `oms panel --register-project PATH --alias {alias}`"
    ))
}

fn resolve_oms_launcher_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::new("HOME must be set to resolve ~/.local/bin/oms"))?;
    require(
        normalized_abs_utf8(&home),
        "HOME must be a normalized UTF-8 absolute path",
    )?;
    Ok(home.join(".local/bin/oms"))
}

fn resolve_codex_marketplace_paths() -> Result<(PathBuf, PathBuf)> {
    let config_root = if let Some(xdg) =
        std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty())
    {
        let p = PathBuf::from(xdg);
        require(
            normalized_abs_utf8(&p),
            "XDG_CONFIG_HOME must be a normalized UTF-8 absolute path",
        )?;
        p
    } else if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(home);
        require(
            normalized_abs_utf8(&p),
            "HOME must be a normalized UTF-8 absolute path",
        )?;
        p.join(".config")
    } else {
        return Err(Error::new(
            "cannot determine user config directory for Codex panels: set XDG_CONFIG_HOME or HOME",
        ));
    };

    let mut ancestor = config_root.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| Error::new("invalid user config directory"))?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error::new("invalid user config ancestor"))?;
            }
            Err(e) => {
                return Err(Error::new(format!(
                    "cannot inspect user config directory {}: {e}",
                    ancestor.display()
                )));
            }
        }
    }
    let mut physical_config = std::fs::canonicalize(ancestor).map_err(|e| {
        Error::new(format!(
            "cannot resolve user config directory {}: {e}",
            ancestor.display()
        ))
    })?;
    for part in missing.iter().rev() {
        physical_config.push(part);
    }
    let oms_config_dir = physical_config.join("oh-my-setting");
    if let Ok(meta) = std::fs::symlink_metadata(&oms_config_dir)
        && meta.file_type().is_symlink()
    {
        return Err(Error::new("refusing symlinked OMS config directory"));
    }
    let marketplace_root = oms_config_dir.join("codex-panels");
    if let Ok(meta) = std::fs::symlink_metadata(&marketplace_root)
        && meta.file_type().is_symlink()
    {
        return Err(Error::new(
            "refusing symlinked Codex panel marketplace directory",
        ));
    }
    require(
        normalized_abs_utf8(&marketplace_root),
        "Codex panel marketplace path must be a normalized UTF-8 absolute path",
    )?;
    Ok((oms_config_dir, marketplace_root))
}

fn resolve_codex_paths_for_active_map(
    target: &Path,
    explicit: bool,
    active_parent: &Path,
    active_leaf: &OsStr,
) -> Result<(PathBuf, PathBuf)> {
    let active_file = active_parent.join(active_leaf);
    let (oms_config_dir, marketplace_root) = match resolve_codex_marketplace_paths() {
        Ok(paths) => paths,
        Err(_) if explicit => {
            return Err(Error::new(format!(
                "Codex panels are supported only for the default user-config project map; refusing active project map {} from OMS_PANEL_PROJECTS_FILE",
                active_file.display()
            )));
        }
        Err(e) => return Err(e),
    };
    let default_map = oms_config_dir.join("desktop-projects.json");
    if explicit {
        if let Some(raw_parent) = target.parent()
            && let Ok(meta) = std::fs::symlink_metadata(raw_parent)
            && meta.file_type().is_symlink()
        {
            return Err(Error::new(
                "refusing symlinked desktop project map directory",
            ));
        }
        if active_file != default_map {
            return Err(Error::new(format!(
                "Codex panels are supported only for the default user-config project map ({}); refusing active project map {}",
                default_map.display(),
                active_file.display()
            )));
        }
    }
    debug_assert_eq!(active_file, default_map);
    Ok((oms_config_dir, marketplace_root))
}

fn codex_marker_value() -> serde_json::Value {
    json!({
        "schema": 1,
        "owner": LIFECYCLE_OWNED_KEY,
        "kind": "codex-host-panels",
        "marketplace": CODEX_MARKETPLACE_NAME,
    })
}

fn codex_marker_bytes() -> Result<Vec<u8>> {
    let mut out = serde_json::to_vec_pretty(&codex_marker_value()).map_err(Error::new)?;
    out.push(b'\n');
    Ok(out)
}

fn verify_codex_marker_at(mp_fd: &OwnedFd, marketplace_root: &Path) -> Result<()> {
    let Some((raw, _)) = read_private_sidecar_at(
        mp_fd,
        OsStr::new(CODEX_MARKETPLACE_MARKER),
        "Codex panel marketplace marker",
        "Codex panel marketplace marker",
    )?
    else {
        return Err(Error::new(format!(
            "unmanaged Codex panel marketplace directory {} lacks OMS ownership marker; refusing to modify",
            marketplace_root.display()
        )));
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|_| {
        Error::new("Invalid Codex panel marketplace marker: expected OMS ownership JSON")
    })?;
    require(
        parsed == codex_marker_value(),
        "Invalid Codex panel marketplace marker: unexpected ownership payload",
    )
}

fn codex_marketplace_manifest_value(aliases: &BTreeSet<String>) -> Result<serde_json::Value> {
    let mut plugins = Vec::with_capacity(aliases.len());
    for alias in aliases {
        let plugin_name = codex_plugin_name(alias)?;
        plugins.push(json!({
            "name": plugin_name,
            "source": {
                "source": "local",
                "path": format!("./plugins/{plugin_name}"),
            },
            "policy": {
                "installation": "AVAILABLE",
                "authentication": "ON_INSTALL",
            },
            "category": "Developer Tools",
        }));
    }
    Ok(json!({
        "name": CODEX_MARKETPLACE_NAME,
        "interface": {
            "displayName": CODEX_MARKETPLACE_DISPLAY_NAME,
        },
        "plugins": plugins,
    }))
}

fn codex_plugin_manifest_value_for_version(
    alias: &str,
    package_version: &str,
) -> Result<serde_json::Value> {
    let plugin_name = codex_plugin_name(alias)?;
    let tpl = codex_plugin_template();
    Ok(json!({
        "$schema": tpl.plugin_schema,
        "name": plugin_name,
        "version": package_version,
        "description": tpl.description,
        "extensions": {
            "com.openai": {
                "requires_local_executor": true,
                "interface": {
                    "displayName": format!("OMS · {alias}"),
                    "shortDescription": tpl.short_description,
                }
            }
        }
    }))
}

fn codex_plugin_manifest_value(alias: &str) -> Result<serde_json::Value> {
    codex_plugin_manifest_value_for_version(alias, OMS_PACKAGE_VERSION)
}

fn codex_mcp_manifest_value_for_version(
    launcher: &Path,
    physical_repo: &Path,
    resource_version: &str,
) -> Result<serde_json::Value> {
    let launcher_str = launcher
        .to_str()
        .ok_or_else(|| Error::new("launcher path must be valid UTF-8"))?;
    let repo_str = physical_repo
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let tpl = codex_plugin_template();
    Ok(json!({
        "$schema": tpl.mcp_schema,
        "mcpServers": {
            PANEL_ENTRY_NAME: {
                "type": "stdio",
                "command": launcher_str,
                "args": ["mcp-server"],
                "env": {
                    "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
                    "OMS_PANEL_REPO": repo_str,
                    "OMS_MCP_PANEL_RESOURCE_VERSION": resource_version,
                }
            }
        }
    }))
}

fn codex_mcp_manifest_value(launcher: &Path, physical_repo: &Path) -> Result<serde_json::Value> {
    codex_mcp_manifest_value_for_version(launcher, physical_repo, codex_panel_resource_version())
}

fn codex_bundle_bytes_match_any_oms_release(
    alias: &str,
    launcher: &Path,
    physical_repo: &Path,
    plugin_bytes: &[u8],
    mcp_bytes: &[u8],
) -> Result<bool> {
    let Ok(plugin_val) = serde_json::from_slice::<serde_json::Value>(plugin_bytes) else {
        return Ok(false);
    };
    let Some(pkg_ver) = plugin_val
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(false);
    };
    let expected_plugin = codex_plugin_manifest_value_for_version(alias, pkg_ver)?;
    let expected_plugin_pretty = serde_json::to_vec_pretty(&expected_plugin).map_err(Error::new)?;
    if plugin_bytes.strip_suffix(b"\n").unwrap_or(plugin_bytes) != expected_plugin_pretty.as_slice()
    {
        return Ok(false);
    }

    let Ok(mcp_val) = serde_json::from_slice::<serde_json::Value>(mcp_bytes) else {
        return Ok(false);
    };
    let Some(res_ver) = mcp_val
        .get("mcpServers")
        .and_then(|s| s.get(PANEL_ENTRY_NAME))
        .and_then(|e| e.get("env"))
        .and_then(|env| env.get("OMS_MCP_PANEL_RESOURCE_VERSION"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(false);
    };
    let expected_mcp = codex_mcp_manifest_value_for_version(launcher, physical_repo, res_ver)?;
    let expected_mcp_pretty = serde_json::to_vec_pretty(&expected_mcp).map_err(Error::new)?;
    Ok(mcp_bytes.strip_suffix(b"\n").unwrap_or(mcp_bytes) == expected_mcp_pretty.as_slice())
}

fn mcp_bytes_pin_checkout(mcp_bytes: &[u8], expected_checkout: &Path) -> bool {
    let Ok(mcp_val) = serde_json::from_slice::<serde_json::Value>(mcp_bytes) else {
        return false;
    };
    let Some(repo_str) = mcp_val
        .get("mcpServers")
        .and_then(|s| s.get(PANEL_ENTRY_NAME))
        .and_then(|e| e.get("env"))
        .and_then(|env| env.get("OMS_PANEL_REPO"))
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let configured = Path::new(repo_str);
    configured == expected_checkout
        || configured.canonicalize().ok().as_deref() == Some(expected_checkout)
}

fn encode_pretty_json(value: &serde_json::Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(Error::new)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_private_dir_stat(stat: &Stat, label: &str) -> Result<()> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(Error::new(format!("refusing symlinked {label}")));
    }
    require(
        kind == FileType::Directory,
        &format!("Invalid {label}: not a directory"),
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {label}: directory is not owned by the current user"),
    )?;
    require(
        stat.st_mode & 0o077 == 0,
        &format!("Invalid {label}: directory must be mode 0700 private to the current user"),
    )?;
    Ok(())
}

fn open_existing_private_subdir_at(
    parent_fd: &OwnedFd,
    name: &OsStr,
    label: &str,
) -> Result<Option<OwnedFd>> {
    match fs::statat(parent_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(None),
        Ok(stat) => {
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind == FileType::Symlink {
                return Err(Error::new(format!("refusing symlinked {label}")));
            }
            require(
                kind == FileType::Directory,
                &format!("Invalid {label}: not a directory"),
            )?;
        }
        Err(e) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    }
    let fd = match fs::openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new(format!("refusing symlinked {label}"))),
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(Error::new(format!("cannot open {label}: {e}"))),
    };
    let stat = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot stat {label}: {e}")))?;
    validate_private_dir_stat(&stat, label)?;
    Ok(Some(fd))
}

fn ensure_private_subdir_at(parent_fd: &OwnedFd, name: &OsStr, label: &str) -> Result<OwnedFd> {
    let created = match fs::mkdirat(parent_fd, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(e) => return Err(Error::new(format!("cannot create {label}: {e}"))),
    };
    if !created && let Ok(stat) = fs::statat(parent_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        let kind = FileType::from_raw_mode(stat.st_mode);
        if kind == FileType::Symlink {
            return Err(Error::new(format!("refusing symlinked {label}")));
        }
        require(
            kind == FileType::Directory,
            &format!("Invalid {label}: not a directory"),
        )?;
    }
    let fd = match fs::openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new(format!("refusing symlinked {label}"))),
        Err(e) => return Err(Error::new(format!("cannot open {label}: {e}"))),
    };
    if created {
        fs::fchmod(&fd, Mode::from_raw_mode(0o700))
            .map_err(|e| Error::new(format!("cannot set 0700 on {label}: {e}")))?;
        sync_dir(parent_fd)?;
    }
    let stat = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot stat {label}: {e}")))?;
    validate_private_dir_stat(&stat, label)?;
    Ok(fd)
}

fn write_private_file_atomic_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
    bytes: &[u8],
    label: &str,
) -> Result<()> {
    let before = match fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            validate_private_sidecar_stat(&stat, label, label)?;
            Some(stat)
        }
        Err(Errno::NOENT) => None,
        Err(e) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    };
    let mut temp = TempMapFile::create(dir_fd, name)?;
    temp.file
        .write_all(bytes)
        .map_err(|e| Error::new(format!("cannot write {label}: {e}")))?;
    fs::fchmod(&temp.file, Mode::from_raw_mode(0o600))
        .map_err(|e| Error::new(format!("cannot set 0600 on {label}: {e}")))?;
    fs::fsync(&temp.file).map_err(|e| Error::new(format!("cannot sync {label}: {e}")))?;
    let staged =
        fs::fstat(&temp.file).map_err(|e| Error::new(format!("cannot verify {label}: {e}")))?;
    validate_private_sidecar_stat(&staged, label, label)?;
    require(
        staged.st_mode & 0o7777 == 0o600,
        &format!("staged {label} did not retain mode 0600"),
    )?;
    require(
        temp.owns_entry(),
        &format!("staged {label} changed before publication"),
    )?;
    match (
        before.as_ref(),
        fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW),
    ) {
        (None, Err(Errno::NOENT)) => {}
        (None, Ok(stat)) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            return Err(Error::new(format!("refusing symlinked {label}")));
        }
        (None, Ok(_)) => {
            return Err(Error::new(format!(
                "{label} was created concurrently; retry"
            )));
        }
        (Some(prev), Ok(curr)) => {
            validate_private_sidecar_stat(&curr, label, label)?;
            require(
                same_stat(prev, &curr),
                &format!("{label} changed concurrently; retry"),
            )?;
        }
        (Some(_), Err(Errno::NOENT)) => {
            return Err(Error::new(format!(
                "{label} disappeared concurrently; retry"
            )));
        }
        (_, Err(e)) => return Err(Error::new(format!("cannot verify {label}: {e}"))),
    }
    fs::renameat(dir_fd, &temp.name, dir_fd, name).map_err(|e| Error {
        code: 1,
        message: format!("cannot publish {label}: {e}"),
        publication_uncertain: true,
    })?;
    temp.published = true;
    sync_dir(dir_fd)
}

fn open_existing_codex_marketplace_fd(
    oms_config_dir: &Path,
    marketplace_root: &Path,
) -> Result<Option<OwnedFd>> {
    let Some(oms_fd) = open_parent_dir(oms_config_dir, false)? else {
        return Ok(None);
    };
    let name = OsStr::new("codex-panels");
    match fs::statat(&oms_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(None),
        Ok(stat) => {
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind == FileType::Symlink {
                return Err(Error::new(
                    "refusing symlinked Codex panel marketplace directory",
                ));
            }
            require(
                kind == FileType::Directory,
                &format!(
                    "Codex panel marketplace path exists and is not a directory: {}",
                    marketplace_root.display()
                ),
            )?;
        }
        Err(e) => {
            return Err(Error::new(format!(
                "cannot inspect Codex panel marketplace directory {}: {e}",
                marketplace_root.display()
            )));
        }
    }
    let mp_fd = match fs::openat(
        &oms_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => {
            return Err(Error::new(
                "refusing symlinked Codex panel marketplace directory",
            ));
        }
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => {
            return Err(Error::new(format!(
                "cannot open Codex panel marketplace directory {}: {e}",
                marketplace_root.display()
            )));
        }
    };
    let stat = fs::fstat(&mp_fd).map_err(|e| {
        Error::new(format!(
            "cannot inspect Codex panel marketplace directory {}: {e}",
            marketplace_root.display()
        ))
    })?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        "Codex panel marketplace directory is not owned by the current user",
    )?;
    // Verify the OMS ownership marker before mode checks so any unmarked existing directory
    // (including 0755 user dirs) is clearly refused as unmanaged.
    verify_codex_marker_at(&mp_fd, marketplace_root)?;
    validate_private_dir_stat(&stat, "Codex panel marketplace directory")?;
    Ok(Some(mp_fd))
}

enum OnDiskCodexBundle {
    Absent,
    ModifiedOutsideOms,
    Present {
        plugin_bytes: Vec<u8>,
        mcp_bytes: Vec<u8>,
        plugin_sha256: String,
        mcp_sha256: String,
        matches_current: bool,
        matches_any_oms_release: bool,
    },
}

struct CodexBundleSnapshot {
    existed_before: bool,
    prior_disk_aliases: BTreeSet<String>,
    expected_market: serde_json::Value,
    market_matches: bool,
    bundle: OnDiskCodexBundle,
}

impl CodexBundleSnapshot {
    fn prior_bundle_pair(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        match &self.bundle {
            OnDiskCodexBundle::Present {
                plugin_bytes,
                mcp_bytes,
                ..
            } => Some((plugin_bytes.clone(), mcp_bytes.clone())),
            OnDiskCodexBundle::Absent | OnDiskCodexBundle::ModifiedOutsideOms => None,
        }
    }
}

fn read_bundle_file_for_compare(
    bundle_fd: &OwnedFd,
    name: &OsStr,
    label: &str,
) -> Result<Option<(Vec<u8>, Stat)>> {
    let named = match fs::statat(bundle_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    };
    let kind = FileType::from_raw_mode(named.st_mode);
    if kind == FileType::Symlink {
        return Err(Error::new(format!("refusing symlinked {label}")));
    }
    require(
        kind == FileType::RegularFile,
        &format!("Invalid {label}: not a regular file"),
    )?;
    require(
        named.st_nlink == 1,
        &format!("Invalid {label}: file must not have hard links"),
    )?;
    require(
        named.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {label}: file is not owned by the current user"),
    )?;
    let fd = match fs::openat(
        bundle_fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new(format!("refusing symlinked {label}"))),
        Err(e) => return Err(Error::new(format!("cannot open {label}: {e}"))),
    };
    let opened = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot inspect {label}: {e}")))?;
    require(
        same_stat(&named, &opened),
        &format!("{label} changed while opening"),
    )?;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(format!("cannot read {label}: {e}")))?;
    let after = fs::fstat(&file).map_err(|e| Error::new(format!("cannot verify {label}: {e}")))?;
    require(
        same_stat(&opened, &after),
        &format!("{label} changed while reading"),
    )?;
    Ok(Some((bytes, after)))
}

fn discover_marked_codex_bundle_aliases_at(
    mp_fd: &OwnedFd,
    marketplace_root: &Path,
) -> Result<BTreeSet<String>> {
    verify_codex_marker_at(mp_fd, marketplace_root)?;
    let Some(plugins_fd) =
        open_existing_private_subdir_at(mp_fd, OsStr::new("plugins"), "Codex plugins directory")?
    else {
        return Ok(BTreeSet::new());
    };
    let plugins_dir = marketplace_root.join("plugins");
    let entries = match std::fs::read_dir(&plugins_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => {
            return Err(Error::new(format!(
                "cannot read Codex plugins directory {}: {e}",
                plugins_dir.display()
            )));
        }
    };
    let mut aliases = BTreeSet::new();
    for entry_res in entries {
        let entry = entry_res.map_err(|e| {
            Error::new(format!(
                "cannot read entry in Codex plugins directory {}: {e}",
                plugins_dir.display()
            ))
        })?;
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let Some(alias) = name_str.strip_prefix("oms-") else {
            continue;
        };
        if !codex_plugin_name(alias).is_ok_and(|expected| expected == name_str) {
            continue;
        }
        let Some(bundle_fd) = open_existing_private_subdir_at(
            &plugins_fd,
            &file_name,
            "Codex plugin bundle directory",
        )?
        else {
            continue;
        };
        let has_plugin = read_bundle_file_for_compare(
            &bundle_fd,
            OsStr::new("plugin.json"),
            "Codex plugin.json",
        )?
        .is_some();
        let has_mcp =
            read_bundle_file_for_compare(&bundle_fd, OsStr::new("mcp.json"), "Codex mcp.json")?
                .is_some();
        if has_plugin && has_mcp {
            aliases.insert(alias.to_owned());
        }
    }
    Ok(aliases)
}

enum CodexBundleUninstallCheck {
    Absent,
    CleanEmptyDir,
    MatchesOms,
    Edited(Vec<PathBuf>),
}

fn inspect_codex_bundle_for_uninstall(
    mp_fd: Option<&OwnedFd>,
    marketplace_root: &Path,
    alias: &str,
    plugin_name: &str,
    launcher: &Path,
    target_repo_path: &Path,
    recorded_hashes: Option<(&str, &str)>,
) -> Result<(CodexBundleUninstallCheck, bool)> {
    let Some(mp_fd) = mp_fd else {
        return Ok((CodexBundleUninstallCheck::Absent, true));
    };
    let Some(plugins_fd) =
        open_existing_private_subdir_at(mp_fd, OsStr::new("plugins"), "Codex plugins directory")?
    else {
        return Ok((CodexBundleUninstallCheck::Absent, true));
    };
    let Some(bundle_fd) = open_existing_private_subdir_at(
        &plugins_fd,
        OsStr::new(plugin_name),
        "Codex plugin bundle directory",
    )?
    else {
        return Ok((CodexBundleUninstallCheck::Absent, true));
    };
    let plugin_file =
        read_bundle_file_for_compare(&bundle_fd, OsStr::new("plugin.json"), "Codex plugin.json")?;
    let mcp_file =
        read_bundle_file_for_compare(&bundle_fd, OsStr::new("mcp.json"), "Codex mcp.json")?;
    if plugin_file.is_none() && mcp_file.is_none() {
        return Ok((CodexBundleUninstallCheck::CleanEmptyDir, true));
    }
    let bundle_dir = marketplace_root.join("plugins").join(plugin_name);
    if let Some((expected_plugin_sha, expected_mcp_sha)) = recorded_hashes {
        let mut edited = Vec::new();
        match &plugin_file {
            Some((bytes, stat)) if oms_panel_core::digest(bytes) == expected_plugin_sha => {
                validate_private_sidecar_stat(stat, "Codex plugin.json", "Codex plugin.json")?;
            }
            _ => edited.push(bundle_dir.join("plugin.json")),
        }
        match &mcp_file {
            Some((bytes, stat)) if oms_panel_core::digest(bytes) == expected_mcp_sha => {
                validate_private_sidecar_stat(stat, "Codex mcp.json", "Codex mcp.json")?;
            }
            _ => edited.push(bundle_dir.join("mcp.json")),
        }
        if edited.is_empty() {
            return Ok((CodexBundleUninstallCheck::MatchesOms, true));
        }
        let checkout_matches = mcp_file
            .as_ref()
            .is_some_and(|(bytes, _)| mcp_bytes_pin_checkout(bytes, target_repo_path));
        return Ok((CodexBundleUninstallCheck::Edited(edited), checkout_matches));
    }

    if let (Some((p_bytes, p_stat)), Some((m_bytes, m_stat))) = (&plugin_file, &mcp_file)
        && codex_bundle_bytes_match_any_oms_release(
            alias,
            launcher,
            target_repo_path,
            p_bytes,
            m_bytes,
        )?
    {
        validate_private_sidecar_stat(p_stat, "Codex plugin.json", "Codex plugin.json")?;
        validate_private_sidecar_stat(m_stat, "Codex mcp.json", "Codex mcp.json")?;
        return Ok((CodexBundleUninstallCheck::MatchesOms, true));
    }
    let edited = vec![bundle_dir.join("plugin.json"), bundle_dir.join("mcp.json")];
    Ok((CodexBundleUninstallCheck::Edited(edited), false))
}

#[allow(clippy::too_many_arguments)]
fn inspect_codex_marketplace_state(
    oms_config_dir: &Path,
    marketplace_root: &Path,
    alias: &str,
    plugin_name: &str,
    launcher: &Path,
    physical_repo: &Path,
    expected_plugin_bytes: &[u8],
    expected_mcp_bytes: &[u8],
) -> Result<CodexBundleSnapshot> {
    let Some(mp_fd) = open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)? else {
        let mut desired = BTreeSet::new();
        desired.insert(alias.to_owned());
        return Ok(CodexBundleSnapshot {
            existed_before: false,
            prior_disk_aliases: BTreeSet::new(),
            expected_market: codex_marketplace_manifest_value(&desired)?,
            market_matches: false,
            bundle: OnDiskCodexBundle::Absent,
        });
    };
    let prior_disk_aliases = discover_marked_codex_bundle_aliases_at(&mp_fd, marketplace_root)?;
    let mut desired_aliases = prior_disk_aliases.clone();
    desired_aliases.insert(alias.to_owned());
    let expected_market = codex_marketplace_manifest_value(&desired_aliases)?;

    let agents_fd =
        open_existing_private_subdir_at(&mp_fd, OsStr::new(".agents"), "Codex .agents directory")?;
    let agents_plugins_fd = match &agents_fd {
        Some(afd) => open_existing_private_subdir_at(
            afd,
            OsStr::new("plugins"),
            "Codex .agents/plugins directory",
        )?,
        None => None,
    };
    let market_matches = match &agents_plugins_fd {
        Some(apfd) => read_private_sidecar_at(
            apfd,
            OsStr::new("marketplace.json"),
            "Codex marketplace manifest",
            "Codex marketplace manifest",
        )?
        .and_then(|(raw, _)| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| v == expected_market),
        None => false,
    };

    let plugins_fd =
        open_existing_private_subdir_at(&mp_fd, OsStr::new("plugins"), "Codex plugins directory")?;
    let bundle_fd = match &plugins_fd {
        Some(pfd) => open_existing_private_subdir_at(
            pfd,
            OsStr::new(plugin_name),
            "Codex plugin bundle directory",
        )?,
        None => None,
    };
    let bundle = if let Some(bfd) = &bundle_fd {
        let p_raw = read_private_sidecar_at(
            bfd,
            OsStr::new("plugin.json"),
            "Codex plugin.json",
            "Codex plugin.json",
        )?;
        let m_raw = read_private_sidecar_at(
            bfd,
            OsStr::new("mcp.json"),
            "Codex mcp.json",
            "Codex mcp.json",
        )?;
        match (p_raw, m_raw) {
            (None, None) => OnDiskCodexBundle::Absent,
            (Some((p_str, _)), Some((m_str, _))) => {
                let plugin_bytes = p_str.into_bytes();
                let mcp_bytes = m_str.into_bytes();
                let plugin_sha256 = oms_panel_core::digest(&plugin_bytes);
                let mcp_sha256 = oms_panel_core::digest(&mcp_bytes);
                let matches_current =
                    plugin_bytes == expected_plugin_bytes && mcp_bytes == expected_mcp_bytes;
                let matches_any_oms_release = codex_bundle_bytes_match_any_oms_release(
                    alias,
                    launcher,
                    physical_repo,
                    &plugin_bytes,
                    &mcp_bytes,
                )?;
                OnDiskCodexBundle::Present {
                    plugin_bytes,
                    mcp_bytes,
                    plugin_sha256,
                    mcp_sha256,
                    matches_current,
                    matches_any_oms_release,
                }
            }
            _ => OnDiskCodexBundle::ModifiedOutsideOms,
        }
    } else {
        OnDiskCodexBundle::Absent
    };

    Ok(CodexBundleSnapshot {
        existed_before: true,
        prior_disk_aliases,
        expected_market,
        market_matches,
        bundle,
    })
}

fn write_codex_bundle_and_manifest(
    oms_config_dir: &Path,
    marketplace_root: &Path,
    plugin_name: &str,
    plugin_bytes: &[u8],
    mcp_bytes: &[u8],
    market_bytes: &[u8],
) -> Result<()> {
    if !oms_config_dir.exists() {
        if let Ok(meta) = std::fs::symlink_metadata(oms_config_dir)
            && meta.file_type().is_symlink()
        {
            return Err(Error::new("refusing symlinked OMS config directory"));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(oms_config_dir)
            .map_err(|e| {
                Error::new(format!(
                    "cannot create OMS config directory {}: {e}",
                    oms_config_dir.display()
                ))
            })?;
    }
    let oms_fd = open_parent_dir(oms_config_dir, false)?
        .ok_or_else(|| Error::new("OMS config directory unavailable"))?;

    let mp_existed = match fs::statat(
        &oms_fd,
        OsStr::new("codex-panels"),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => true,
        Err(Errno::NOENT) => false,
        Err(e) => {
            return Err(Error::new(format!(
                "cannot inspect Codex panel marketplace directory {}: {e}",
                marketplace_root.display()
            )));
        }
    };
    let mp_fd = if mp_existed {
        open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)?
            .ok_or_else(|| Error::new("Codex panel marketplace directory disappeared"))?
    } else {
        let fd = ensure_private_subdir_at(
            &oms_fd,
            OsStr::new("codex-panels"),
            "Codex panel marketplace directory",
        )?;
        let marker = codex_marker_bytes()?;
        write_private_file_atomic_at(
            &fd,
            OsStr::new(CODEX_MARKETPLACE_MARKER),
            &marker,
            "Codex panel marketplace marker",
        )?;
        fd
    };

    let agents_fd =
        ensure_private_subdir_at(&mp_fd, OsStr::new(".agents"), "Codex .agents directory")?;
    let agents_plugins_fd = ensure_private_subdir_at(
        &agents_fd,
        OsStr::new("plugins"),
        "Codex .agents/plugins directory",
    )?;
    let plugins_fd =
        ensure_private_subdir_at(&mp_fd, OsStr::new("plugins"), "Codex plugins directory")?;
    let bundle_fd = ensure_private_subdir_at(
        &plugins_fd,
        OsStr::new(plugin_name),
        "Codex plugin bundle directory",
    )?;

    write_private_file_atomic_at(
        &bundle_fd,
        OsStr::new("plugin.json"),
        plugin_bytes,
        "Codex plugin.json",
    )?;
    write_private_file_atomic_at(
        &bundle_fd,
        OsStr::new("mcp.json"),
        mcp_bytes,
        "Codex mcp.json",
    )?;
    write_private_file_atomic_at(
        &agents_plugins_fd,
        OsStr::new("marketplace.json"),
        market_bytes,
        "Codex marketplace manifest",
    )
}

fn unlink_if_exists(dir_fd: &OwnedFd, name: &OsStr, flags: AtFlags, label: &str) -> Result<()> {
    match fs::unlinkat(dir_fd, name, flags) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(Error::new(format!("cannot remove {label}: {e}"))),
    }
}

fn describe_rmdir_error(err: Errno, dir_path: &Path) -> String {
    if matches!(err, Errno::NOTEMPTY | Errno::EXIST) {
        if let Ok(entries) = std::fs::read_dir(dir_path) {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name != CODEX_MARKETPLACE_MARKER)
                .map(|name| render::clean(&name, 80))
                .filter(|name| !name.is_empty())
                .collect();
            names.sort();
            names.truncate(8);
            if !names.is_empty() {
                return format!("directory is not empty (leftover: {})", names.join(", "));
            }
        }
        return format!("directory is not empty ({err})");
    }
    err.to_string()
}

fn rmdir_reporting_leftovers(
    parent_fd: &OwnedFd,
    name: &OsStr,
    full_path: &Path,
    label: &str,
) -> Result<()> {
    match fs::unlinkat(parent_fd, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(Error::new(format!(
            "cannot remove {label} {}: {}",
            full_path.display(),
            describe_rmdir_error(e, full_path)
        ))),
    }
}

fn restore_codex_marker_at(oms_fd: &OwnedFd, marker_bytes: &[u8]) -> Result<()> {
    let mp_fd = fs::openat(
        oms_fd,
        OsStr::new("codex-panels"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        Error::new(format!(
            "cannot reopen Codex panel marketplace directory: {e}"
        ))
    })?;
    let stat = fs::fstat(&mp_fd).map_err(|e| {
        Error::new(format!(
            "cannot stat Codex panel marketplace directory: {e}"
        ))
    })?;
    validate_private_dir_stat(&stat, "Codex panel marketplace directory")?;
    write_private_file_atomic_at(
        &mp_fd,
        OsStr::new(CODEX_MARKETPLACE_MARKER),
        marker_bytes,
        "Codex panel marketplace marker",
    )
}

fn remove_codex_bundle_dir_at(
    mp_fd: &OwnedFd,
    marketplace_root: &Path,
    plugin_name: &str,
) -> Result<bool> {
    let Some(plugins_fd) =
        open_existing_private_subdir_at(mp_fd, OsStr::new("plugins"), "Codex plugins directory")?
    else {
        return Ok(false);
    };
    if let Some(bundle_fd) = open_existing_private_subdir_at(
        &plugins_fd,
        OsStr::new(plugin_name),
        "Codex plugin bundle directory",
    )? {
        unlink_if_exists(
            &bundle_fd,
            OsStr::new("plugin.json"),
            AtFlags::empty(),
            "Codex plugin.json",
        )?;
        unlink_if_exists(
            &bundle_fd,
            OsStr::new("mcp.json"),
            AtFlags::empty(),
            "Codex mcp.json",
        )?;
        sync_dir(&bundle_fd)?;
        drop(bundle_fd);
        rmdir_reporting_leftovers(
            &plugins_fd,
            OsStr::new(plugin_name),
            &marketplace_root.join("plugins").join(plugin_name),
            "Codex plugin bundle directory",
        )?;
        sync_dir(&plugins_fd)?;
        return Ok(true);
    }
    Ok(false)
}

fn update_or_delete_codex_marketplace_disk(
    oms_config_dir: &Path,
    marketplace_root: &Path,
    remove_plugin_name: Option<&str>,
    remaining_disk_aliases: &BTreeSet<String>,
    keep_marketplace_root: bool,
) -> Result<bool> {
    let Some(mp_fd) = open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)? else {
        return Ok(false);
    };
    let mut removed_any = false;
    if let Some(plugin_name) = remove_plugin_name
        && remove_codex_bundle_dir_at(&mp_fd, marketplace_root, plugin_name)?
    {
        removed_any = true;
    }
    if !remaining_disk_aliases.is_empty() || keep_marketplace_root {
        let agents_fd =
            ensure_private_subdir_at(&mp_fd, OsStr::new(".agents"), "Codex .agents directory")?;
        let agents_plugins_fd = ensure_private_subdir_at(
            &agents_fd,
            OsStr::new("plugins"),
            "Codex .agents/plugins directory",
        )?;
        let market_bytes =
            encode_pretty_json(&codex_marketplace_manifest_value(remaining_disk_aliases)?)?;
        write_private_file_atomic_at(
            &agents_plugins_fd,
            OsStr::new("marketplace.json"),
            &market_bytes,
            "Codex marketplace manifest",
        )?;
        return Ok(removed_any);
    }

    // Re-verify ownership marker before deleting marketplace files; remove the marker last so an
    // unexpected leftover entry never leaves an unmarked directory behind.
    verify_codex_marker_at(&mp_fd, marketplace_root)?;
    let marker_backup = codex_marker_bytes()?;
    if let Some(agents_fd) =
        open_existing_private_subdir_at(&mp_fd, OsStr::new(".agents"), "Codex .agents directory")?
    {
        if let Some(agents_plugins_fd) = open_existing_private_subdir_at(
            &agents_fd,
            OsStr::new("plugins"),
            "Codex .agents/plugins directory",
        )? {
            unlink_if_exists(
                &agents_plugins_fd,
                OsStr::new("marketplace.json"),
                AtFlags::empty(),
                "Codex marketplace manifest",
            )?;
            sync_dir(&agents_plugins_fd)?;
            drop(agents_plugins_fd);
            rmdir_reporting_leftovers(
                &agents_fd,
                OsStr::new("plugins"),
                &marketplace_root.join(".agents/plugins"),
                "Codex .agents/plugins directory",
            )?;
        }
        sync_dir(&agents_fd)?;
        drop(agents_fd);
        rmdir_reporting_leftovers(
            &mp_fd,
            OsStr::new(".agents"),
            &marketplace_root.join(".agents"),
            "Codex .agents directory",
        )?;
    }
    if open_existing_private_subdir_at(&mp_fd, OsStr::new("plugins"), "Codex plugins directory")?
        .is_some()
    {
        rmdir_reporting_leftovers(
            &mp_fd,
            OsStr::new("plugins"),
            &marketplace_root.join("plugins"),
            "Codex plugins directory",
        )?;
    }
    unlink_if_exists(
        &mp_fd,
        OsStr::new(CODEX_MARKETPLACE_MARKER),
        AtFlags::empty(),
        "Codex panel marketplace marker",
    )?;
    sync_dir(&mp_fd)?;
    drop(mp_fd);

    if let Some(oms_fd) = open_parent_dir(oms_config_dir, false)? {
        match fs::unlinkat(&oms_fd, OsStr::new("codex-panels"), AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {
                sync_dir(&oms_fd)?;
            }
            Err(e) => {
                let restore_err = restore_codex_marker_at(&oms_fd, &marker_backup).err();
                let detail = describe_rmdir_error(e, marketplace_root);
                if let Some(re) = restore_err {
                    return Err(Error {
                        code: 1,
                        message: format!(
                            "cannot remove Codex panel marketplace directory {}: {detail}; also failed restoring ownership marker ({})",
                            marketplace_root.display(),
                            re.message
                        ),
                        publication_uncertain: true,
                    });
                }
                return Err(Error::new(format!(
                    "cannot remove Codex panel marketplace directory {}: {detail}",
                    marketplace_root.display()
                )));
            }
        }
    }
    Ok(true)
}

fn claude_panel_entry_json(launcher: &Path, physical_repo: &Path) -> Result<String> {
    debug_assert_ne!(PANEL_ENTRY_NAME, LIFECYCLE_OWNED_KEY);
    let launcher_str = launcher
        .to_str()
        .ok_or_else(|| Error::new("launcher path must be valid UTF-8"))?;
    let repo_str = physical_repo
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let entry = json!({
        "type": "stdio",
        "command": launcher_str,
        "args": ["mcp-server"],
        "env": {
            "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
            "OMS_PANEL_REPO": repo_str,
        }
    });
    serde_json::to_string(&entry).map_err(Error::new)
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && fs::access(path, fs::Access::EXEC_OK).is_ok()
}

fn find_on_path(binary: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &dir
            };
            let candidate = dir.join(binary);
            if !is_executable(&candidate) {
                return None;
            }
            dir.canonicalize()
                .ok()
                .map(|parent| parent.join(binary))
                .or(Some(candidate))
        })
    })
}

fn resolve_vendor_program(app: PanelApp, cwd: &Path, dry_run: bool) -> Result<PathBuf> {
    let (env_var, default_bin) = app.bin_env_and_default();
    let label = app.cli_label();
    if let Some(raw) = std::env::var_os(env_var).filter(|v| !v.is_empty()) {
        require(
            raw.to_str().is_some() && !raw.as_bytes().contains(&0),
            &format!("{env_var} must be valid UTF-8"),
        )?;
        let candidate = PathBuf::from(&raw);
        let has_sep = candidate.components().count() > 1 || candidate.is_absolute();
        let resolved = if has_sep {
            if candidate.is_absolute() {
                candidate
            } else {
                cwd.join(candidate)
            }
        } else if dry_run {
            return Ok(candidate);
        } else {
            find_on_path(&candidate).ok_or_else(|| {
                Error::new(format!(
                    "{label} '{}' from {env_var} was not found on PATH",
                    candidate.display()
                ))
            })?
        };
        if !dry_run && !is_executable(&resolved) {
            return Err(Error::new(format!(
                "{label} '{}' from {env_var} is not an executable file",
                resolved.display()
            )));
        }
        return Ok(resolved);
    }
    if dry_run {
        return Ok(PathBuf::from(default_bin));
    }
    find_on_path(Path::new(default_bin)).ok_or_else(|| {
        Error::new(format!(
            "{label} '{default_bin}' was not found on PATH (or set {env_var})"
        ))
    })
}

fn shell_quote(raw: &str) -> String {
    if !raw.is_empty()
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:@=-".contains(&b))
    {
        raw.to_owned()
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

fn format_vendor_command(workdir: &Path, program: &Path, args: &[&str]) -> Result<String> {
    let dir_str = workdir
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let prog_str = program
        .to_str()
        .ok_or_else(|| Error::new("vendor CLI path must be valid UTF-8"))?;
    let mut line = format!("cd {} && {}", shell_quote(dir_str), shell_quote(prog_str));
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    line.push('\n');
    Ok(line)
}

struct CmdOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

enum VendorExecError {
    Spawn(Error),
    TimedOut(Error),
    Interrupted(Error),
    Wait(Error),
}

impl VendorExecError {
    fn into_error(self) -> Error {
        match self {
            Self::Spawn(e) | Self::TimedOut(e) | Self::Interrupted(e) | Self::Wait(e) => e,
        }
    }
}

fn vendor_timeout(app: PanelApp) -> Duration {
    let keys: &[&str] = match app {
        PanelApp::Claude => &["OMS_CLAUDE_TIMEOUT_MS"],
        PanelApp::Codex => &["OMS_CODEX_TIMEOUT_MS", "OMS_CLAUDE_TIMEOUT_MS"],
    };
    for key in keys {
        if let Some(raw) = std::env::var_os(key)
            && let Some(s) = raw.to_str()
            && let Ok(ms) = s.trim().parse::<u64>()
            && (20..=300_000).contains(&ms)
        {
            return Duration::from_millis(ms);
        }
    }
    VENDOR_TIMEOUT
}

fn format_timeout(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!("{}s", timeout.as_secs())
    } else {
        format!("{}ms", timeout.as_millis())
    }
}

struct ChildGroup {
    child: std::process::Child,
    pgid: Option<Pid>,
    label: &'static str,
    reaped: bool,
}

impl ChildGroup {
    fn check_exited(&mut self) -> std::result::Result<Option<std::process::ExitStatus>, Error> {
        let label = self.label;
        if let Some(pgid) = self.pgid {
            loop {
                match waitid(
                    WaitId::Pid(pgid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(info)) if info.exited() || info.killed() || info.dumped() => {
                        let _ = kill_process_group(pgid, Signal::KILL);
                        self.reaped = true;
                        return self
                            .child
                            .wait()
                            .map(Some)
                            .map_err(|e| Error::new(format!("failed waiting for {label}: {e}")));
                    }
                    Ok(_) => return Ok(None),
                    Err(Errno::INTR) => continue,
                    Err(Errno::INVAL | Errno::NOSYS) => break,
                    Err(e) => {
                        return Err(Error::new(format!("failed waiting for {label}: {e}")));
                    }
                }
            }
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                if let Some(pgid) = self.pgid {
                    let _ = kill_process_group(pgid, Signal::KILL);
                }
                self.reaped = true;
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::new(format!("failed waiting for {label}: {e}"))),
        }
    }

    fn kill_and_reap(&mut self) {
        if self.reaped {
            return;
        }
        if let Some(pgid) = self.pgid {
            let _ = kill_process_group(pgid, Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

fn set_nonblocking(fd: impl AsFd, label: &str) -> Result<()> {
    let flags =
        fcntl_getfl(&fd).map_err(|e| Error::new(format!("cannot inspect {label} pipe: {e}")))?;
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK)
        .map_err(|e| Error::new(format!("cannot configure {label} pipe: {e}")))
}

fn drain_pipe(pipe: &mut impl Read, buf: &mut Vec<u8>, label: &str) -> Result<bool> {
    let mut chunk = [0u8; 8192];
    for _ in 0..64 {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                let remaining = MAX_CAPTURE_BYTES.saturating_sub(buf.len());
                if remaining > 0 {
                    buf.extend_from_slice(&chunk[..n.min(remaining)]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::new(format!("failed reading {label} pipe: {e}"))),
        }
    }
    Ok(false)
}

fn run_bounded_command(
    app: PanelApp,
    program: &Path,
    args: &[&str],
    workdir: &Path,
    stop: &AtomicBool,
) -> std::result::Result<CmdOutput, VendorExecError> {
    let label = app.cli_label();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| VendorExecError::Spawn(Error::new(format!("cannot execute {label}: {e}"))))?;

    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| VendorExecError::Wait(Error::new("missing child stdout pipe")))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| VendorExecError::Wait(Error::new("missing child stderr pipe")))?;

    let pgid = i32::try_from(child.id())
        .ok()
        .filter(|p| *p > 0)
        .and_then(Pid::from_raw);
    let mut group = ChildGroup {
        child,
        pgid,
        label,
        reaped: false,
    };

    set_nonblocking(&stdout_pipe, label).map_err(VendorExecError::Wait)?;
    set_nonblocking(&stderr_pipe, label).map_err(VendorExecError::Wait)?;

    let timeout = vendor_timeout(app);
    let deadline = Instant::now() + timeout;
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let mut out_eof = false;
    let mut err_eof = false;
    let mut exit_status = None;

    loop {
        if stop.load(Ordering::Acquire) {
            group.kill_and_reap();
            return Err(VendorExecError::Interrupted(Error {
                code: 130,
                message: format!("interrupted while running {label}"),
                publication_uncertain: false,
            }));
        }

        if !out_eof {
            out_eof =
                drain_pipe(&mut stdout_pipe, &mut out_buf, label).map_err(VendorExecError::Wait)?;
        }
        if !err_eof {
            err_eof =
                drain_pipe(&mut stderr_pipe, &mut err_buf, label).map_err(VendorExecError::Wait)?;
        }

        if exit_status.is_none() {
            match group.check_exited() {
                Ok(Some(status)) => {
                    exit_status = Some(status);
                    if !out_eof {
                        out_eof = drain_pipe(&mut stdout_pipe, &mut out_buf, label)
                            .map_err(VendorExecError::Wait)?;
                    }
                    if !err_eof {
                        err_eof = drain_pipe(&mut stderr_pipe, &mut err_buf, label)
                            .map_err(VendorExecError::Wait)?;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    group.kill_and_reap();
                    return Err(VendorExecError::Wait(e));
                }
            }
        }

        if let Some(status) = exit_status {
            if (out_eof && err_eof) || Instant::now() >= deadline {
                return Ok(CmdOutput {
                    code: status.code(),
                    stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                    stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                });
            }
        } else if Instant::now() >= deadline {
            group.kill_and_reap();
            return Err(VendorExecError::TimedOut(Error::new(format!(
                "{label} timed out after {}",
                format_timeout(timeout)
            ))));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        let slice = remaining.min(Duration::from_millis(10));
        let ts = Timespec {
            tv_sec: i64::try_from(slice.as_secs()).unwrap_or(0),
            tv_nsec: slice.subsec_nanos() as _,
        };
        let mut poll_fds = Vec::with_capacity(2);
        if !out_eof {
            poll_fds.push(PollFd::new(&stdout_pipe, PollFlags::IN));
        }
        if !err_eof {
            poll_fds.push(PollFd::new(&stderr_pipe, PollFlags::IN));
        }
        if poll_fds.is_empty() {
            std::thread::sleep(slice);
        } else {
            let _ = poll(&mut poll_fds, Some(&ts));
        }
    }
}

fn sanitized_vendor_reason(out: &CmdOutput) -> Option<String> {
    let env_pairs: Vec<(Vec<u8>, Vec<u8>)> = std::env::vars_os()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
    let env_refs: Vec<(&[u8], &[u8])> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    for src in [&out.stderr, &out.stdout] {
        for raw_line in src.lines() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("WARNING:")
                || trimmed.contains('{')
                || trimmed.contains('}')
                || trimmed.contains('=')
            {
                continue;
            }
            if let Ok(scrubbed) = oms_scrub::redact(trimmed.as_bytes(), &env_refs)
                && let Ok(text) = std::str::from_utf8(&scrubbed.bytes)
            {
                let clean = render::clean(text.trim(), 160);
                if !clean.is_empty() {
                    return Some(clean);
                }
            }
        }
    }
    None
}

fn vendor_failure(prefix: &str, subcommand: &str, out: &CmdOutput) -> Error {
    let status = out
        .code
        .map_or_else(|| "terminated by signal".into(), |c| format!("exit {c}"));
    match sanitized_vendor_reason(out) {
        Some(reason) => Error::new(format!("{prefix} {subcommand} failed ({status}): {reason}")),
        None => Error::new(format!("{prefix} {subcommand} failed ({status})")),
    }
}

fn is_mcp_entry_missing(out: &CmdOutput) -> bool {
    if out.code != Some(MCP_MISSING_STATUS) {
        return false;
    }
    let combined = format!("{}\n{}", out.stderr, out.stdout).to_ascii_lowercase();
    let entry_lower = PANEL_ENTRY_NAME.to_ascii_lowercase();
    if !combined.contains(&entry_lower) || combined.contains("command not found") {
        return false;
    }
    let direct = [
        "no mcp server",
        "no local mcp server",
        "no local-scoped mcp server",
        "no project-scoped mcp server",
        "no user-scoped mcp server",
        "no server named",
        "unknown server",
        "oms-panel not found",
        "\"oms-panel\" not found",
        "'oms-panel' not found",
        "oms-panel does not exist",
        "\"oms-panel\" does not exist",
        "'oms-panel' does not exist",
    ];
    if direct.iter().any(|needle| combined.contains(needle)) {
        return true;
    }
    (combined.contains("server") || combined.contains("entry"))
        && (combined.contains("not found") || combined.contains("does not exist"))
}

fn is_codex_plugin_missing(plugin_id: &str, plugin_name: &str, out: &CmdOutput) -> bool {
    if out.code != Some(1) {
        return false;
    }
    let combined = format!("{}\n{}", out.stderr, out.stdout).to_ascii_lowercase();
    if combined.contains("command not found") {
        return false;
    }
    let id_lower = plugin_id.to_ascii_lowercase();
    let name_lower = plugin_name.to_ascii_lowercase();
    (combined.contains(&id_lower) || combined.contains(&name_lower))
        && (combined.contains("not installed")
            || combined.contains("not found")
            || combined.contains("does not exist")
            || combined.contains("not configured"))
}

fn is_codex_marketplace_missing(out: &CmdOutput) -> bool {
    if out.code != Some(1) {
        return false;
    }
    let combined = format!("{}\n{}", out.stderr, out.stdout).to_ascii_lowercase();
    !combined.contains("command not found")
        && combined.contains(CODEX_MARKETPLACE_NAME)
        && (combined.contains("not configured")
            || combined.contains("not installed")
            || combined.contains("not found")
            || combined.contains("does not exist"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexInstalledPlugin {
    plugin_id: String,
    name: Option<String>,
    marketplace_name: Option<String>,
    installed: bool,
    enabled: Option<bool>,
    source_path: Option<PathBuf>,
}

impl CodexInstalledPlugin {
    fn matches_expected(&self, marketplace_root: &Path, plugin_name: &str) -> bool {
        let expected_id = format!("{plugin_name}@{CODEX_MARKETPLACE_NAME}");
        let expected_source = marketplace_root.join("plugins").join(plugin_name);
        self.installed
            && self.enabled != Some(false)
            && self.plugin_id == expected_id
            && self.name.as_deref().is_none_or(|n| n == plugin_name)
            && self
                .marketplace_name
                .as_deref()
                .is_none_or(|m| m == CODEX_MARKETPLACE_NAME)
            && self.source_path.as_ref().is_none_or(|p| {
                p == &expected_source || p.canonicalize().ok().as_ref() == Some(&expected_source)
            })
    }
}

struct CodexInventory {
    plugins: BTreeMap<String, CodexInstalledPlugin>,
    marketplaces: BTreeMap<String, PathBuf>,
}

fn parse_codex_plugin_list(stdout: &str) -> Option<BTreeMap<String, CodexInstalledPlugin>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let obj = val.as_object()?;
    let installed_arr = obj.get("installed")?.as_array()?;
    if !obj.get("available").is_none_or(serde_json::Value::is_array) {
        return None;
    }
    let mut plugins = BTreeMap::new();
    for item in installed_arr {
        let row = item.as_object()?;
        let plugin_id = row.get("pluginId")?.as_str()?;
        if plugin_id.is_empty() {
            return None;
        }
        let name = match row.get("name") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.as_str()?.to_owned()),
        };
        let marketplace_name = match row.get("marketplaceName") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.as_str()?.to_owned()),
        };
        let installed = row.get("installed")?.as_bool()?;
        let enabled = match row.get("enabled") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.as_bool()?),
        };
        if let Some(v) = row.get("version")
            && !v.is_null()
        {
            v.as_str()?;
        }
        let source_path = match row.get("source") {
            None | Some(serde_json::Value::Null) => None,
            Some(src) => match src.as_object()?.get("path") {
                None | Some(serde_json::Value::Null) => None,
                Some(p) => Some(PathBuf::from(p.as_str()?)),
            },
        };
        if plugins
            .insert(
                plugin_id.to_owned(),
                CodexInstalledPlugin {
                    plugin_id: plugin_id.to_owned(),
                    name,
                    marketplace_name,
                    installed,
                    enabled,
                    source_path,
                },
            )
            .is_some()
        {
            return None;
        }
    }
    Some(plugins)
}

fn valid_marketplace_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn valid_marketplace_root_spec(raw: &str) -> bool {
    raw.starts_with('/')
        || raw.starts_with("https://")
        || raw.starts_with("http://")
        || raw.starts_with("ssh://")
        || raw.starts_with("file://")
        || raw.starts_with("git@")
}

fn parse_codex_marketplace_list(stdout: &str) -> Option<BTreeMap<String, PathBuf>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("no plugin marketplaces in scope.")
        || trimmed.eq_ignore_ascii_case("no plugin marketplaces configured.")
        || trimmed.eq_ignore_ascii_case("no marketplaces configured.")
    {
        return Some(BTreeMap::new());
    }
    let mut marketplaces = BTreeMap::new();
    let mut saw_first = false;
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let end = line.find(char::is_whitespace)?;
        let name = &line[..end];
        let rest = line[end..].trim();
        if !saw_first {
            saw_first = true;
            if name.eq_ignore_ascii_case("marketplace") && rest.eq_ignore_ascii_case("root") {
                continue;
            }
        }
        if !valid_marketplace_token(name) || rest.is_empty() || !valid_marketplace_root_spec(rest) {
            return None;
        }
        if name == CODEX_MARKETPLACE_NAME && !rest.starts_with('/') {
            return None;
        }
        if let Some(prev) = marketplaces.insert(name.to_owned(), PathBuf::from(rest))
            && prev != Path::new(rest)
        {
            return None;
        }
    }
    Some(marketplaces)
}

fn probe_codex_inventory(
    codex_bin: &Path,
    workdir: &Path,
    stop: &AtomicBool,
) -> Result<CodexInventory> {
    let list_out = run_bounded_command(
        PanelApp::Codex,
        codex_bin,
        &["plugin", "list", "--json"],
        workdir,
        stop,
    )
    .map_err(VendorExecError::into_error)?;
    if list_out.code != Some(0) {
        return Err(vendor_failure("codex plugin", "list", &list_out));
    }
    let plugins = parse_codex_plugin_list(&list_out.stdout)
        .ok_or_else(|| Error::new("cannot parse `codex plugin list --json` output"))?;

    let mp_out = run_bounded_command(
        PanelApp::Codex,
        codex_bin,
        &["plugin", "marketplace", "list"],
        workdir,
        stop,
    )
    .map_err(VendorExecError::into_error)?;
    if mp_out.code != Some(0) {
        return Err(vendor_failure("codex plugin", "marketplace list", &mp_out));
    }
    let marketplaces = parse_codex_marketplace_list(&mp_out.stdout)
        .ok_or_else(|| Error::new("cannot parse `codex plugin marketplace list` output"))?;

    Ok(CodexInventory {
        plugins,
        marketplaces,
    })
}

fn recorded_codex_aliases(panels: &PanelMap) -> BTreeSet<String> {
    panels
        .iter()
        .filter(|(_, rows)| rows.iter().any(|r| r.app == PanelApp::Codex.as_str()))
        .map(|(alias, _)| alias.clone())
        .collect()
}

fn same_marketplace_root(configured: &Path, expected_physical: &Path) -> bool {
    configured == expected_physical
        || configured.canonicalize().ok().as_deref() == Some(expected_physical)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeScope {
    Local,
    User,
    Project,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ClaudeMcpEntry {
    scope: Option<ClaudeScope>,
    transport_type: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    has_extra_fields: bool,
}

impl ClaudeMcpEntry {
    fn matches_expected(&self, launcher: &Path, physical_repo: &Path) -> bool {
        let Some(launcher_str) = launcher.to_str() else {
            return false;
        };
        let Some(repo_str) = physical_repo.to_str() else {
            return false;
        };
        !self.has_extra_fields
            && self.scope == Some(ClaudeScope::Local)
            && self.transport_type.as_deref().is_none_or(|t| t == "stdio")
            && self.command.as_deref() == Some(launcher_str)
            && self.args == ["mcp-server"]
            && self.env.len() == 2
            && self.env.get("OMS_MCP_TOOL_PROFILE").map(String::as_str) == Some("chatgpt-panel")
            && self.env.get("OMS_PANEL_REPO").map(String::as_str) == Some(repo_str)
    }
}

fn classify_scope_token(raw: &str) -> Option<ClaudeScope> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("local") {
        Some(ClaudeScope::Local)
    } else if lower.starts_with("user") {
        Some(ClaudeScope::User)
    } else if lower.starts_with("project") {
        Some(ClaudeScope::Project)
    } else {
        None
    }
}

fn parse_claude_mcp_get(stdout: &str) -> Option<ClaudeMcpEntry> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') {
        let val = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
        let root_obj = val.as_object()?;
        let cfg_val = val.get("config").unwrap_or(&val);
        let cfg = cfg_val.as_object()?;
        let scope = val
            .get("scope")
            .or_else(|| cfg_val.get("scope"))
            .and_then(|v| v.as_str())
            .and_then(classify_scope_token)?;
        let mut has_extra_fields = false;
        if val.get("config").is_some()
            && root_obj
                .keys()
                .any(|k| !matches!(k.as_str(), "scope" | "config" | "name" | "status"))
        {
            has_extra_fields = true;
        }
        if cfg.keys().any(|k| {
            !matches!(
                k.as_str(),
                "scope" | "type" | "command" | "args" | "env" | "status"
            )
        }) {
            has_extra_fields = true;
        }
        let transport_type = match cfg.get("type") {
            Some(v) => Some(v.as_str()?.to_owned()),
            None => None,
        };
        let command = match cfg.get("command") {
            Some(v) => Some(v.as_str()?.to_owned()),
            None => None,
        };
        let args = match cfg.get("args") {
            Some(v) => v
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        let mut env = BTreeMap::new();
        if let Some(env_val) = cfg.get("env") {
            for (k, v) in env_val.as_object()? {
                env.insert(k.clone(), v.as_str()?.to_owned());
            }
        }
        if command.is_none() && transport_type.is_none() && !has_extra_fields {
            return None;
        }
        return Some(ClaudeMcpEntry {
            scope: Some(scope),
            transport_type,
            command,
            args,
            env,
            has_extra_fields,
        });
    }

    let mut entry = ClaudeMcpEntry::default();
    let mut in_env = false;
    let mut env_indent = 0usize;
    let mut has_detail = false;
    for line in stdout.lines() {
        let indent = line
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count();
        let body = line.trim();
        if body.is_empty() {
            in_env = false;
            continue;
        }
        if in_env && indent > env_indent {
            let (k, v) = body.split_once('=')?;
            let key = k.trim();
            if key.is_empty() {
                return None;
            }
            if entry.env.insert(key.to_owned(), v.to_owned()).is_some() {
                entry.has_extra_fields = true;
            }
            continue;
        }
        in_env = false;

        if let Some((label, rest)) = body.split_once(':') {
            let key = label.trim().to_ascii_lowercase();
            let value = rest.trim();
            match key.as_str() {
                "scope" => {
                    entry.scope = Some(classify_scope_token(value)?);
                }
                "type" => {
                    entry.transport_type = Some(value.to_owned());
                    has_detail = true;
                }
                "command" => {
                    entry.command = Some(value.to_owned());
                    has_detail = true;
                }
                "args" => {
                    entry.args = if value.is_empty() || value == "(none)" || value == "none" {
                        Vec::new()
                    } else {
                        value.split_whitespace().map(str::to_owned).collect()
                    };
                }
                "environment" if value.is_empty() => {
                    in_env = true;
                    env_indent = indent;
                }
                "status" | "to remove this server, run" => {}
                _ if indent == 0 && value.is_empty() => {}
                _ if indent > 0 => {
                    entry.has_extra_fields = true;
                    has_detail = true;
                }
                _ => {}
            }
        }
        if entry.scope.is_none() {
            let lower = body.to_ascii_lowercase();
            if lower.contains("claude mcp remove") {
                if lower.ends_with("-s local") || lower.contains("--scope local") {
                    entry.scope = Some(ClaudeScope::Local);
                } else if lower.ends_with("-s user") || lower.contains("--scope user") {
                    entry.scope = Some(ClaudeScope::User);
                } else if lower.ends_with("-s project") || lower.contains("--scope project") {
                    entry.scope = Some(ClaudeScope::Project);
                }
            }
        }
    }
    (entry.scope.is_some() && has_detail).then_some(entry)
}

fn probe_claude_entry(
    claude_bin: &Path,
    workdir: &Path,
    stop: &AtomicBool,
) -> Result<Option<ClaudeMcpEntry>> {
    let out = run_bounded_command(
        PanelApp::Claude,
        claude_bin,
        &["mcp", "get", PANEL_ENTRY_NAME],
        workdir,
        stop,
    )
    .map_err(VendorExecError::into_error)?;
    if out.code == Some(0) {
        return parse_claude_mcp_get(&out.stdout)
            .map(Some)
            .ok_or_else(|| Error::new("cannot parse `claude mcp get oms-panel` output"));
    }
    if is_mcp_entry_missing(&out) {
        return Ok(None);
    }
    Err(vendor_failure("claude mcp", "get", &out))
}

fn panel_action_json(
    app: PanelApp,
    alias: &str,
    path: &Path,
    entry: &str,
    action: &str,
    changed: bool,
) -> serde_json::Value {
    json!({
        "app": app.as_str(),
        "alias": alias,
        "path": path,
        "entry": entry,
        "action": action,
        "changed": changed,
    })
}

fn upsert_panel_row(
    panels: &mut PanelMap,
    alias: &str,
    app: PanelApp,
    entry_name: &str,
    physical: &Path,
    codex_hashes: Option<(String, String)>,
) {
    let (plugin_sha256, mcp_sha256) = match codex_hashes {
        Some((p, m)) => (Some(p), Some(m)),
        None => (None, None),
    };
    let rows = panels.entry(alias.to_owned()).or_default();
    if let Some(slot) = rows.iter_mut().find(|r| r.app == app.as_str()) {
        slot.name = entry_name.to_owned();
        slot.path = physical.to_path_buf();
        slot.plugin_sha256 = plugin_sha256;
        slot.mcp_sha256 = mcp_sha256;
    } else {
        rows.push(PanelRecord {
            app: app.as_str().to_owned(),
            scope: app.expected_scope().to_owned(),
            name: entry_name.to_owned(),
            path: physical.to_path_buf(),
            plugin_sha256,
            mcp_sha256,
        });
    }
}

fn drop_panel_row(panels: &mut PanelMap, alias: &str, app: PanelApp) {
    if let Some(rows) = panels.get_mut(alias) {
        rows.retain(|r| r.app != app.as_str());
        if rows.is_empty() {
            panels.remove(alias);
        }
    }
}

fn rollback_after_claude_add(
    want_json: bool,
    app: PanelApp,
    alias: &str,
    physical: &Path,
    claude_bin: &Path,
    primary: Error,
) -> Result<Output> {
    let rollback_stop = AtomicBool::new(false);
    let rollback = run_bounded_command(
        PanelApp::Claude,
        claude_bin,
        &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
        physical,
        &rollback_stop,
    );
    let rollback_err_msg = match rollback {
        Ok(rb) if rb.code == Some(0) || is_mcp_entry_missing(&rb) => {
            return Err(Error {
                code: primary.code,
                message: format!(
                    "{}; rolled back local {PANEL_ENTRY_NAME} entry",
                    primary.message
                ),
                publication_uncertain: primary.publication_uncertain,
            });
        }
        Ok(rb) => vendor_failure("claude mcp", "remove", &rb).message,
        Err(e) => e.into_error().message,
    };
    let code = if primary.code != 0 { primary.code } else { 1 };
    let message = format!(
        "{}; rollback via `claude mcp remove --scope local {PANEL_ENTRY_NAME}` failed ({rollback_err_msg}): local {PANEL_ENTRY_NAME} state is uncertain",
        primary.message
    );
    let err = Error {
        code,
        message: message.clone(),
        publication_uncertain: true,
    };
    if want_json {
        let (_, _, stderr) = crate::failure(err);
        let mut report =
            panel_action_json(app, alias, physical, PANEL_ENTRY_NAME, "install", false);
        report["uncertain"] = json!(true);
        report["error"] = json!(message);
        return Ok((code, json(&report)?, stderr));
    }
    Err(err)
}

#[allow(clippy::too_many_arguments)]
fn rollback_codex_install(
    want_json: bool,
    alias: &str,
    plugin_name: &str,
    plugin_id: &str,
    physical: &Path,
    codex_bin: &Path,
    oms_config_dir: &Path,
    marketplace_root: &Path,
    prior_disk_aliases: &BTreeSet<String>,
    prior_bundle_files: Option<&(Vec<u8>, Vec<u8>)>,
    plugin_was_installed_before: bool,
    attempted_plugin_add: bool,
    market_was_configured_before: bool,
    added_market_now: bool,
    other_installed_in_market: bool,
    announce_rollback: bool,
    primary: Error,
) -> Result<Output> {
    let rollback_stop = AtomicBool::new(false);
    let mut rb_errors = Vec::new();

    if attempted_plugin_add && !plugin_was_installed_before {
        match run_bounded_command(
            PanelApp::Codex,
            codex_bin,
            &["plugin", "remove", plugin_id],
            physical,
            &rollback_stop,
        ) {
            Ok(rb)
                if rb.code == Some(0) || is_codex_plugin_missing(plugin_id, plugin_name, &rb) => {}
            Ok(rb) => rb_errors.push(vendor_failure("codex plugin", "remove", &rb).message),
            Err(e) => rb_errors.push(e.into_error().message),
        }
    }

    let mut remaining_disk = prior_disk_aliases.clone();
    if !plugin_was_installed_before {
        remaining_disk.remove(alias);
    }

    if rb_errors.is_empty()
        && added_market_now
        && remaining_disk.is_empty()
        && !other_installed_in_market
    {
        match run_bounded_command(
            PanelApp::Codex,
            codex_bin,
            &["plugin", "marketplace", "remove", CODEX_MARKETPLACE_NAME],
            physical,
            &rollback_stop,
        ) {
            Ok(rb) if rb.code == Some(0) || is_codex_marketplace_missing(&rb) => {}
            Ok(rb) => {
                rb_errors.push(vendor_failure("codex plugin", "marketplace remove", &rb).message)
            }
            Err(e) => rb_errors.push(e.into_error().message),
        }
    }

    // If `codex plugin remove` (or `marketplace remove`) failed or was uncertain, keep the bundle
    // and marketplace entry on disk so a later uninstall can clean up.
    if rb_errors.is_empty() {
        let disk_res = if plugin_was_installed_before
            && let Some((prev_plugin, prev_mcp)) = prior_bundle_files
        {
            encode_pretty_json(&codex_marketplace_manifest_value(prior_disk_aliases)?).and_then(
                |market_bytes| {
                    write_codex_bundle_and_manifest(
                        oms_config_dir,
                        marketplace_root,
                        plugin_name,
                        prev_plugin,
                        prev_mcp,
                        &market_bytes,
                    )
                },
            )
        } else {
            let keep_root = market_was_configured_before || other_installed_in_market;
            update_or_delete_codex_marketplace_disk(
                oms_config_dir,
                marketplace_root,
                Some(plugin_name),
                &remaining_disk,
                keep_root,
            )
            .map(|_| ())
        };
        if let Err(e) = disk_res {
            rb_errors.push(e.message);
        }
    }

    if rb_errors.is_empty() {
        let message = if announce_rollback {
            format!("{}; rolled back Codex panel {plugin_id}", primary.message)
        } else {
            primary.message
        };
        return Err(Error {
            code: primary.code,
            message,
            publication_uncertain: primary.publication_uncertain,
        });
    }

    let code = if primary.code != 0 { primary.code } else { 1 };
    let message = format!(
        "{}; Codex panel rollback for {plugin_id} failed ({}): Codex panel state is uncertain",
        primary.message,
        rb_errors.join("; ")
    );
    let err = Error {
        code,
        message: message.clone(),
        publication_uncertain: true,
    };
    if want_json {
        let (_, _, stderr) = crate::failure(err);
        let mut report = panel_action_json(
            PanelApp::Codex,
            alias,
            physical,
            plugin_id,
            "install",
            false,
        );
        report["uncertain"] = json!(true);
        report["error"] = json!(message);
        return Ok((code, json(&report)?, stderr));
    }
    Err(err)
}

pub(super) fn install_panel(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let app = PanelApp::parse(&options.required("install-panel")?)?;
    let alias = options.required("project")?;
    check_alias(&alias)?;
    let entry_name = app.expected_entry_name(&alias)?;
    let dry_run = options.has("dry-run");
    let want_json = options.has("json");

    let (target, explicit) = resolve_writer_path()?;
    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
    let codex_paths = if app == PanelApp::Codex {
        Some(resolve_codex_paths_for_active_map(
            &target, explicit, &parent, &leaf_name,
        )?)
    } else {
        None
    };
    let launcher = resolve_oms_launcher_path()?;
    let Some(dir_fd) = open_parent_dir(&parent, false)? else {
        return Err(if explicit {
            Error::new(INVALID_MAP_MSG)
        } else {
            unknown_alias_error(&alias)
        });
    };
    if let Ok(stat) = fs::statat(&dir_fd, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
        validate_map_stat(&stat)?;
    }
    if let Ok(stat) = fs::statat(
        &dir_fd,
        OsStr::new(PANELS_FILE_NAME),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        validate_panels_stat(&stat)?;
    }

    if dry_run {
        let Some((map, _)) = read_map_at(&dir_fd, &leaf_name)? else {
            return Err(if explicit {
                Error::new(INVALID_MAP_MSG)
            } else {
                unknown_alias_error(&alias)
            });
        };
        let _ = read_panels_at(&dir_fd)?;
        let configured = map.get(&alias).ok_or_else(|| unknown_alias_error(&alias))?;
        let configured_str = configured
            .to_str()
            .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
        let physical = validate_checkout(Some(configured_str), cwd)?;
        match app {
            PanelApp::Claude => {
                let claude_bin = resolve_vendor_program(app, cwd, true)?;
                let entry_json = claude_panel_entry_json(&launcher, &physical)?;
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &physical, &entry_name, "install", false);
                    return Ok((0, json(&report)?, vec![]));
                }
                let mut text = format_vendor_command(
                    &physical,
                    &claude_bin,
                    &["mcp", "get", PANEL_ENTRY_NAME],
                )?;
                text.push_str(&format_vendor_command(
                    &physical,
                    &claude_bin,
                    &[
                        "mcp",
                        "add-json",
                        "--scope",
                        "local",
                        PANEL_ENTRY_NAME,
                        &entry_json,
                    ],
                )?);
                return Ok((0, text.into_bytes(), vec![]));
            }
            PanelApp::Codex => {
                let (oms_config_dir, marketplace_root) = codex_paths.as_ref().unwrap();
                require(
                    !marketplace_root.starts_with(&physical)
                        && !physical.starts_with(marketplace_root),
                    "Codex panel marketplace directory must remain outside the project checkout",
                )?;
                let _ = open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)?;
                let codex_bin = resolve_vendor_program(app, cwd, true)?;
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &physical, &entry_name, "install", false);
                    return Ok((0, json(&report)?, vec![]));
                }
                let mp_str = marketplace_root
                    .to_str()
                    .ok_or_else(|| Error::new("marketplace path must be valid UTF-8"))?;
                let mut text =
                    format_vendor_command(&physical, &codex_bin, &["plugin", "list", "--json"])?;
                text.push_str(&format_vendor_command(
                    &physical,
                    &codex_bin,
                    &["plugin", "marketplace", "list"],
                )?);
                text.push_str(&format_vendor_command(
                    &physical,
                    &codex_bin,
                    &["plugin", "marketplace", "add", mp_str],
                )?);
                text.push_str(&format_vendor_command(
                    &physical,
                    &codex_bin,
                    &["plugin", "add", &entry_name],
                )?);
                return Ok((0, text.into_bytes(), vec![]));
            }
        }
    }

    let lock_target = parent.join(&leaf_name);
    let _guard = lock_map_file(&lock_target, cwd, stop)?;

    let Some((map, _)) = read_map_at(&dir_fd, &leaf_name)? else {
        return Err(if explicit {
            Error::new(INVALID_MAP_MSG)
        } else {
            unknown_alias_error(&alias)
        });
    };
    let configured = map.get(&alias).ok_or_else(|| unknown_alias_error(&alias))?;
    let configured_str = configured
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let physical = validate_checkout(Some(configured_str), cwd)?;

    let (mut panels, before_panels_stat) = match read_panels_at(&dir_fd)? {
        Some((panels, stat)) => (panels, Some(stat)),
        None => (BTreeMap::new(), None),
    };
    let recorded_row = panels
        .get(&alias)
        .and_then(|rows| rows.iter().find(|r| r.app == app.as_str()))
        .cloned();
    if let Some(existing) = &recorded_row
        && existing.path != physical
    {
        return Err(Error::new(format!(
            "project alias '{alias}' already has a recorded {} panel at {}; run `oms panel --uninstall-panel {} --project {alias}` first",
            app.as_str(),
            existing.path.display(),
            app.as_str(),
        )));
    }

    match app {
        PanelApp::Claude => {
            let claude_bin = resolve_vendor_program(app, cwd, false)?;
            let entry_json = claude_panel_entry_json(&launcher, &physical)?;
            if let Some(found) = probe_claude_entry(&claude_bin, &physical, stop)?
                && found.scope == Some(ClaudeScope::Local)
            {
                if !found.matches_expected(&launcher, &physical) {
                    return Err(Error::new(
                        "an unmanaged local oms-panel entry exists; remove it first",
                    ));
                }
                if recorded_row.is_some() {
                    if want_json {
                        let report = panel_action_json(
                            app,
                            &alias,
                            &physical,
                            &entry_name,
                            "install",
                            false,
                        );
                        return Ok((0, json(&report)?, vec![]));
                    }
                    return Ok((
                        0,
                        format!(
                            "already installed {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                            app.as_str(),
                            physical.display()
                        )
                        .into_bytes(),
                        vec![],
                    ));
                }
                upsert_panel_row(&mut panels, &alias, app, &entry_name, &physical, None);
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &physical, &entry_name, "adopted", true);
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!(
                        "adopted existing {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                        app.as_str(),
                        physical.display()
                    )
                    .into_bytes(),
                    vec![],
                ));
            }

            if stop.load(Ordering::Acquire) {
                return Err(Error {
                    code: 130,
                    message: "interrupted before running Claude CLI".into(),
                    publication_uncertain: false,
                });
            }

            let add_res = run_bounded_command(
                app,
                &claude_bin,
                &[
                    "mcp",
                    "add-json",
                    "--scope",
                    "local",
                    PANEL_ENTRY_NAME,
                    &entry_json,
                ],
                &physical,
                stop,
            );
            match add_res {
                Ok(add_out) if add_out.code == Some(0) => {}
                Ok(add_out) if add_out.code.is_some() => {
                    return Err(vendor_failure("claude mcp", "add-json", &add_out));
                }
                Ok(sig_out) => {
                    return rollback_after_claude_add(
                        want_json,
                        app,
                        &alias,
                        &physical,
                        &claude_bin,
                        vendor_failure("claude mcp", "add-json", &sig_out),
                    );
                }
                Err(VendorExecError::Spawn(e)) => return Err(e),
                Err(
                    VendorExecError::TimedOut(primary)
                    | VendorExecError::Interrupted(primary)
                    | VendorExecError::Wait(primary),
                ) => {
                    return rollback_after_claude_add(
                        want_json,
                        app,
                        &alias,
                        &physical,
                        &claude_bin,
                        primary,
                    );
                }
            }

            upsert_panel_row(&mut panels, &alias, app, &entry_name, &physical, None);
            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                return rollback_after_claude_add(
                    want_json,
                    app,
                    &alias,
                    &physical,
                    &claude_bin,
                    record_err,
                );
            }

            if want_json {
                let report =
                    panel_action_json(app, &alias, &physical, &entry_name, "install", true);
                return Ok((0, json(&report)?, vec![]));
            }
            Ok((
                0,
                format!(
                    "installed {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                    app.as_str(),
                    physical.display()
                )
                .into_bytes(),
                vec![],
            ))
        }
        PanelApp::Codex => {
            let (oms_config_dir, marketplace_root) = codex_paths.as_ref().unwrap();
            require(
                !marketplace_root.starts_with(&physical) && !physical.starts_with(marketplace_root),
                "Codex panel marketplace directory must remain outside the project checkout",
            )?;
            let plugin_name = codex_plugin_name(&alias)?;
            debug_assert_ne!(CODEX_MARKETPLACE_NAME, LIFECYCLE_CODEX_MARKETPLACE);

            let expected_plugin = codex_plugin_manifest_value(&alias)?;
            let expected_mcp = codex_mcp_manifest_value(&launcher, &physical)?;
            let plugin_bytes = encode_pretty_json(&expected_plugin)?;
            let mcp_bytes = encode_pretty_json(&expected_mcp)?;

            // Check on-disk marketplace directory ownership and marked bundles before calling Codex CLI.
            let disk_state = inspect_codex_marketplace_state(
                oms_config_dir,
                marketplace_root,
                &alias,
                &plugin_name,
                &launcher,
                &physical,
                &plugin_bytes,
                &mcp_bytes,
            )?;
            let prior_bundle_files = disk_state.prior_bundle_pair();

            if let Some(existing) = &recorded_row {
                match &disk_state.bundle {
                    OnDiskCodexBundle::ModifiedOutsideOms => {
                        return Err(Error::new(format!(
                            "Codex panel bundle for '{alias}' was modified outside OMS; restore it or run `oms panel --uninstall-panel codex --project {alias}` first"
                        )));
                    }
                    OnDiskCodexBundle::Present {
                        plugin_sha256,
                        mcp_sha256,
                        ..
                    } if existing.plugin_sha256.as_deref() != Some(plugin_sha256.as_str())
                        || existing.mcp_sha256.as_deref() != Some(mcp_sha256.as_str()) =>
                    {
                        return Err(Error::new(format!(
                            "Codex panel bundle for '{alias}' was modified outside OMS; restore it or run `oms panel --uninstall-panel codex --project {alias}` first"
                        )));
                    }
                    OnDiskCodexBundle::Absent | OnDiskCodexBundle::Present { .. } => {}
                }
            }

            let codex_bin = resolve_vendor_program(app, cwd, false)?;
            let inventory = probe_codex_inventory(&codex_bin, &physical, stop)?;
            let market_was_configured = if let Some(configured_root) =
                inventory.marketplaces.get(CODEX_MARKETPLACE_NAME)
            {
                if !same_marketplace_root(configured_root, marketplace_root) {
                    return Err(Error::new(format!(
                        "an unmanaged Codex marketplace '{CODEX_MARKETPLACE_NAME}' is configured at {}; remove it first",
                        configured_root.display()
                    )));
                }
                true
            } else {
                false
            };

            let plugin_was_installed_before = inventory
                .plugins
                .get(&entry_name)
                .is_some_and(|p| p.installed);
            let other_installed_in_market = inventory.plugins.values().any(|p| {
                p.installed
                    && p.plugin_id != entry_name
                    && p.plugin_id
                        .rsplit_once('@')
                        .is_some_and(|(_, market)| market == CODEX_MARKETPLACE_NAME)
            });

            let market_bytes = encode_pretty_json(&disk_state.expected_market)?;

            if recorded_row.is_some() {
                if let Some(found_plugin) =
                    inventory.plugins.get(&entry_name).filter(|p| p.installed)
                {
                    if !found_plugin.matches_expected(marketplace_root, &plugin_name) {
                        return Err(Error::new(format!(
                            "an unmanaged Codex plugin {entry_name} exists; remove it first"
                        )));
                    }
                    if market_was_configured
                        && disk_state.existed_before
                        && disk_state.market_matches
                        && matches!(
                            disk_state.bundle,
                            OnDiskCodexBundle::Present {
                                matches_current: true,
                                ..
                            }
                        )
                    {
                        if want_json {
                            let report = panel_action_json(
                                app,
                                &alias,
                                &physical,
                                &entry_name,
                                "install",
                                false,
                            );
                            return Ok((0, json(&report)?, vec![]));
                        }
                        return Ok((
                            0,
                            format!(
                                "already installed {} panel {entry_name} for {alias}: {}\n",
                                app.as_str(),
                                physical.display()
                            )
                            .into_bytes(),
                            vec![],
                        ));
                    }
                }
            } else if let Some(found_plugin) =
                inventory.plugins.get(&entry_name).filter(|p| p.installed)
            {
                if market_was_configured
                    && disk_state.existed_before
                    && found_plugin.matches_expected(marketplace_root, &plugin_name)
                    && let OnDiskCodexBundle::Present {
                        ref plugin_bytes,
                        ref mcp_bytes,
                        ref plugin_sha256,
                        ref mcp_sha256,
                        matches_any_oms_release: true,
                        ..
                    } = disk_state.bundle
                {
                    if !disk_state.market_matches {
                        write_codex_bundle_and_manifest(
                            oms_config_dir,
                            marketplace_root,
                            &plugin_name,
                            plugin_bytes,
                            mcp_bytes,
                            &market_bytes,
                        )?;
                    }
                    upsert_panel_row(
                        &mut panels,
                        &alias,
                        app,
                        &entry_name,
                        &physical,
                        Some((plugin_sha256.clone(), mcp_sha256.clone())),
                    );
                    write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
                    if want_json {
                        let report =
                            panel_action_json(app, &alias, &physical, &entry_name, "adopted", true);
                        return Ok((0, json(&report)?, vec![]));
                    }
                    return Ok((
                        0,
                        format!(
                            "adopted existing {} panel {entry_name} for {alias}: {}\n",
                            app.as_str(),
                            physical.display()
                        )
                        .into_bytes(),
                        vec![],
                    ));
                }
                return Err(Error::new(format!(
                    "an unmanaged Codex plugin {entry_name} exists; remove it first"
                )));
            } else if matches!(
                disk_state.bundle,
                OnDiskCodexBundle::ModifiedOutsideOms
                    | OnDiskCodexBundle::Present {
                        matches_any_oms_release: false,
                        ..
                    }
            ) {
                return Err(Error::new(format!(
                    "an unmanaged Codex plugin bundle for '{alias}' exists at {}; remove it first",
                    marketplace_root
                        .join("plugins")
                        .join(&plugin_name)
                        .display()
                )));
            }

            if stop.load(Ordering::Acquire) {
                return Err(Error {
                    code: 130,
                    message: "interrupted before running Codex CLI".into(),
                    publication_uncertain: false,
                });
            }

            // 1. Write or refresh the bundle for ALIAS and the marketplace manifest.
            write_codex_bundle_and_manifest(
                oms_config_dir,
                marketplace_root,
                &plugin_name,
                &plugin_bytes,
                &mcp_bytes,
                &market_bytes,
            )?;

            // 2. If the marketplace is not yet configured, run `codex plugin marketplace add <root>`.
            let mut added_market_now = false;
            if !market_was_configured {
                let mp_str = marketplace_root
                    .to_str()
                    .ok_or_else(|| Error::new("marketplace path must be valid UTF-8"))?;
                let mp_res = run_bounded_command(
                    app,
                    &codex_bin,
                    &["plugin", "marketplace", "add", mp_str],
                    &physical,
                    stop,
                );
                match mp_res {
                    Ok(out) if out.code == Some(0) => {
                        added_market_now = true;
                    }
                    Ok(out) if out.code.is_some() => {
                        return rollback_codex_install(
                            want_json,
                            &alias,
                            &plugin_name,
                            &entry_name,
                            &physical,
                            &codex_bin,
                            oms_config_dir,
                            marketplace_root,
                            &disk_state.prior_disk_aliases,
                            prior_bundle_files.as_ref(),
                            plugin_was_installed_before,
                            false,
                            market_was_configured,
                            false,
                            other_installed_in_market,
                            false,
                            vendor_failure("codex plugin", "marketplace add", &out),
                        );
                    }
                    Ok(sig_out) => {
                        return rollback_codex_install(
                            want_json,
                            &alias,
                            &plugin_name,
                            &entry_name,
                            &physical,
                            &codex_bin,
                            oms_config_dir,
                            marketplace_root,
                            &disk_state.prior_disk_aliases,
                            prior_bundle_files.as_ref(),
                            plugin_was_installed_before,
                            false,
                            market_was_configured,
                            true,
                            other_installed_in_market,
                            true,
                            vendor_failure("codex plugin", "marketplace add", &sig_out),
                        );
                    }
                    Err(VendorExecError::Spawn(e)) => {
                        return rollback_codex_install(
                            want_json,
                            &alias,
                            &plugin_name,
                            &entry_name,
                            &physical,
                            &codex_bin,
                            oms_config_dir,
                            marketplace_root,
                            &disk_state.prior_disk_aliases,
                            prior_bundle_files.as_ref(),
                            plugin_was_installed_before,
                            false,
                            market_was_configured,
                            false,
                            other_installed_in_market,
                            false,
                            e,
                        );
                    }
                    Err(
                        VendorExecError::TimedOut(primary)
                        | VendorExecError::Interrupted(primary)
                        | VendorExecError::Wait(primary),
                    ) => {
                        return rollback_codex_install(
                            want_json,
                            &alias,
                            &plugin_name,
                            &entry_name,
                            &physical,
                            &codex_bin,
                            oms_config_dir,
                            marketplace_root,
                            &disk_state.prior_disk_aliases,
                            prior_bundle_files.as_ref(),
                            plugin_was_installed_before,
                            false,
                            market_was_configured,
                            true,
                            other_installed_in_market,
                            true,
                            primary,
                        );
                    }
                }
            }

            // 3. Run `codex plugin add oms-<alias>@oms-host-panels` (also required on refresh so
            //    Codex updates its cached bundle copy under `$CODEX_HOME/plugins/cache/...`).
            let add_res = run_bounded_command(
                app,
                &codex_bin,
                &["plugin", "add", &entry_name],
                &physical,
                stop,
            );
            match add_res {
                Ok(out) if out.code == Some(0) => {}
                Ok(out) => {
                    return rollback_codex_install(
                        want_json,
                        &alias,
                        &plugin_name,
                        &entry_name,
                        &physical,
                        &codex_bin,
                        oms_config_dir,
                        marketplace_root,
                        &disk_state.prior_disk_aliases,
                        prior_bundle_files.as_ref(),
                        plugin_was_installed_before,
                        true,
                        market_was_configured,
                        added_market_now,
                        other_installed_in_market,
                        true,
                        vendor_failure("codex plugin", "add", &out),
                    );
                }
                Err(VendorExecError::Spawn(e)) => {
                    return rollback_codex_install(
                        want_json,
                        &alias,
                        &plugin_name,
                        &entry_name,
                        &physical,
                        &codex_bin,
                        oms_config_dir,
                        marketplace_root,
                        &disk_state.prior_disk_aliases,
                        prior_bundle_files.as_ref(),
                        plugin_was_installed_before,
                        false,
                        market_was_configured,
                        added_market_now,
                        other_installed_in_market,
                        added_market_now,
                        e,
                    );
                }
                Err(
                    VendorExecError::TimedOut(primary)
                    | VendorExecError::Interrupted(primary)
                    | VendorExecError::Wait(primary),
                ) => {
                    return rollback_codex_install(
                        want_json,
                        &alias,
                        &plugin_name,
                        &entry_name,
                        &physical,
                        &codex_bin,
                        oms_config_dir,
                        marketplace_root,
                        &disk_state.prior_disk_aliases,
                        prior_bundle_files.as_ref(),
                        plugin_was_installed_before,
                        true,
                        market_was_configured,
                        added_market_now,
                        other_installed_in_market,
                        true,
                        primary,
                    );
                }
            }

            // 4. Record or update the row and written bundle digests after success.
            let new_hashes = (
                oms_panel_core::digest(&plugin_bytes),
                oms_panel_core::digest(&mcp_bytes),
            );
            upsert_panel_row(
                &mut panels,
                &alias,
                app,
                &entry_name,
                &physical,
                Some(new_hashes),
            );
            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                return rollback_codex_install(
                    want_json,
                    &alias,
                    &plugin_name,
                    &entry_name,
                    &physical,
                    &codex_bin,
                    oms_config_dir,
                    marketplace_root,
                    &disk_state.prior_disk_aliases,
                    prior_bundle_files.as_ref(),
                    plugin_was_installed_before,
                    true,
                    market_was_configured,
                    added_market_now,
                    other_installed_in_market,
                    true,
                    record_err,
                );
            }

            if want_json {
                let report =
                    panel_action_json(app, &alias, &physical, &entry_name, "install", true);
                return Ok((0, json(&report)?, vec![]));
            }
            Ok((
                0,
                format!(
                    "installed {} panel {entry_name} for {alias}: {}\n",
                    app.as_str(),
                    physical.display()
                )
                .into_bytes(),
                vec![],
            ))
        }
    }
}

pub(super) fn uninstall_panel(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let app = PanelApp::parse(&options.required("uninstall-panel")?)?;
    let alias = options.required("project")?;
    check_alias(&alias)?;
    let entry_name = app.expected_entry_name(&alias)?;
    let dry_run = options.has("dry-run");
    let want_json = options.has("json");

    let (target, explicit) = resolve_writer_path()?;
    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
    let codex_paths = if app == PanelApp::Codex {
        Some(resolve_codex_paths_for_active_map(
            &target, explicit, &parent, &leaf_name,
        )?)
    } else {
        None
    };
    let Some(dir_fd) = open_parent_dir(&parent, false)? else {
        return Err(if explicit {
            Error::new(INVALID_MAP_MSG)
        } else {
            unknown_alias_error(&alias)
        });
    };
    if let Ok(stat) = fs::statat(&dir_fd, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
        validate_map_stat(&stat)?;
    }
    if let Ok(stat) = fs::statat(
        &dir_fd,
        OsStr::new(PANELS_FILE_NAME),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        validate_panels_stat(&stat)?;
    }

    if dry_run {
        let Some((map, _)) = read_map_at(&dir_fd, &leaf_name)? else {
            return Err(if explicit {
                Error::new(INVALID_MAP_MSG)
            } else {
                unknown_alias_error(&alias)
            });
        };
        let configured = map.get(&alias).ok_or_else(|| unknown_alias_error(&alias))?;
        let panels = read_panels_at(&dir_fd)?.map(|(p, _)| p).unwrap_or_default();
        let recorded = panels
            .get(&alias)
            .and_then(|rows| rows.iter().find(|r| r.app == app.as_str()))
            .cloned();
        let codex_disk_aliases = if let Some((oms_config_dir, marketplace_root)) = &codex_paths {
            match open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)? {
                Some(mp_fd) => discover_marked_codex_bundle_aliases_at(&mp_fd, marketplace_root)?,
                None => BTreeSet::new(),
            }
        } else {
            BTreeSet::new()
        };
        let target_path = if let Some(row) = &recorded {
            row.path.clone()
        } else if app == PanelApp::Codex && codex_disk_aliases.contains(&alias) {
            std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone())
        } else {
            let report_path =
                std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
            if want_json {
                let report =
                    panel_action_json(app, &alias, &report_path, &entry_name, "uninstall", false);
                return Ok((0, json(&report)?, vec![]));
            }
            return Ok((
                0,
                format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                vec![],
            ));
        };
        let checkout_exists = std::fs::metadata(&target_path).is_ok_and(|m| m.is_dir());
        match app {
            PanelApp::Claude => {
                if !checkout_exists {
                    if want_json {
                        let report = panel_action_json(
                            app,
                            &alias,
                            &target_path,
                            &entry_name,
                            "forgotten",
                            false,
                        );
                        return Ok((0, json(&report)?, vec![]));
                    }
                    return Ok((
                        0,
                        format!(
                            "{} panel {PANEL_ENTRY_NAME} for {alias} ({}): checkout is gone; would drop the record (the Claude entry stored for that path is inert)\n",
                            app.as_str(),
                            target_path.display()
                        )
                        .into_bytes(),
                        vec![],
                    ));
                }
                let claude_bin = resolve_vendor_program(app, cwd, true)?;
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &target_path,
                        &entry_name,
                        "uninstall",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                let mut text = format_vendor_command(
                    &target_path,
                    &claude_bin,
                    &["mcp", "get", PANEL_ENTRY_NAME],
                )?;
                text.push_str(&format_vendor_command(
                    &target_path,
                    &claude_bin,
                    &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                )?);
                return Ok((0, text.into_bytes(), vec![]));
            }
            PanelApp::Codex => {
                let codex_bin = resolve_vendor_program(app, cwd, true)?;
                let action = if checkout_exists {
                    "uninstall"
                } else {
                    "forgotten"
                };
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &target_path, &entry_name, action, false);
                    return Ok((0, json(&report)?, vec![]));
                }
                let exec_dir = if checkout_exists {
                    &target_path
                } else {
                    &parent
                };
                let mut remaining = recorded_codex_aliases(&panels);
                remaining.extend(codex_disk_aliases);
                remaining.remove(&alias);
                let mut text = format_vendor_command(
                    exec_dir,
                    &codex_bin,
                    &["plugin", "remove", &entry_name],
                )?;
                if remaining.is_empty() {
                    text.push_str(&format_vendor_command(
                        exec_dir,
                        &codex_bin,
                        &["plugin", "marketplace", "remove", CODEX_MARKETPLACE_NAME],
                    )?);
                }
                return Ok((0, text.into_bytes(), vec![]));
            }
        }
    }

    let lock_target = parent.join(&leaf_name);
    let _guard = lock_map_file(&lock_target, cwd, stop)?;

    let Some((map, _)) = read_map_at(&dir_fd, &leaf_name)? else {
        return Err(if explicit {
            Error::new(INVALID_MAP_MSG)
        } else {
            unknown_alias_error(&alias)
        });
    };
    let configured = map.get(&alias).ok_or_else(|| unknown_alias_error(&alias))?;
    let (mut panels, before_panels_stat) = match read_panels_at(&dir_fd)? {
        Some((panels, stat)) => (panels, Some(stat)),
        None => (BTreeMap::new(), None),
    };
    let recorded = panels
        .get(&alias)
        .and_then(|rows| rows.iter().find(|r| r.app == app.as_str()))
        .cloned();
    if let Some(row) = &recorded {
        require(
            row.scope == app.expected_scope() && row.name == entry_name,
            "Invalid OMS project panel map: unexpected scope or entry name",
        )?;
    }

    enum UninstallDisposition {
        Removed,
        AlreadyGone,
        LeftModifiedClaude,
        LeftModifiedCodex {
            edited_files: Vec<PathBuf>,
            removed_plugin: bool,
        },
    }

    match app {
        PanelApp::Claude => {
            let Some(row) = recorded else {
                let report_path =
                    std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &report_path,
                        &entry_name,
                        "uninstall",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                    vec![],
                ));
            };
            let checkout_exists = std::fs::metadata(&row.path).is_ok_and(|m| m.is_dir());
            if !checkout_exists {
                drop_panel_row(&mut panels, &alias, app);
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &row.path, &entry_name, "forgotten", true);
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!(
                        "{} panel {PANEL_ENTRY_NAME} for {alias} ({}): checkout is gone; dropped the record (the Claude entry stored for that path is inert)\n",
                        app.as_str(),
                        row.path.display()
                    )
                    .into_bytes(),
                    vec![],
                ));
            }
            let launcher = resolve_oms_launcher_path()?;
            let claude_bin = resolve_vendor_program(app, cwd, false)?;
            let disposition = match probe_claude_entry(&claude_bin, &row.path, stop)? {
                Some(found) if found.scope == Some(ClaudeScope::Local) => {
                    if found.matches_expected(&launcher, &row.path) {
                        let out = run_bounded_command(
                            app,
                            &claude_bin,
                            &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                            &row.path,
                            stop,
                        )
                        .map_err(VendorExecError::into_error)?;
                        if out.code == Some(0) {
                            UninstallDisposition::Removed
                        } else if is_mcp_entry_missing(&out) {
                            UninstallDisposition::AlreadyGone
                        } else {
                            return Err(vendor_failure("claude mcp", "remove", &out));
                        }
                    } else {
                        UninstallDisposition::LeftModifiedClaude
                    }
                }
                Some(_) | None => UninstallDisposition::AlreadyGone,
            };
            drop_panel_row(&mut panels, &alias, app);
            write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
            if want_json {
                let report =
                    panel_action_json(app, &alias, &row.path, &entry_name, "uninstall", true);
                return Ok((0, json(&report)?, vec![]));
            }
            let msg = match disposition {
                UninstallDisposition::Removed => format!(
                    "uninstalled {} panel {entry_name} for {alias}: {}\n",
                    app.as_str(),
                    row.path.display()
                ),
                UninstallDisposition::AlreadyGone => format!(
                    "{} panel {entry_name} for {alias} was already gone; dropped ownership record ({})\n",
                    app.as_str(),
                    row.path.display()
                ),
                UninstallDisposition::LeftModifiedClaude => format!(
                    "the local {PANEL_ENTRY_NAME} entry was changed outside OMS; left it in place and dropped ownership record for {alias} ({})\n",
                    row.path.display()
                ),
                UninstallDisposition::LeftModifiedCodex { .. } => unreachable!(),
            };
            Ok((0, msg.into_bytes(), vec![]))
        }
        PanelApp::Codex => {
            let (oms_config_dir, marketplace_root) = codex_paths.as_ref().unwrap();
            let plugin_name = codex_plugin_name(&alias)?;
            // If no row is recorded and `codex-panels` does not exist on disk, exit cleanly as a
            // no-op; if `codex-panels` exists, verify its marker and check for a leftover bundle
            // kept after an earlier uncertain rollback.
            if recorded.is_none()
                && std::fs::symlink_metadata(marketplace_root)
                    .err()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                let report_path =
                    std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &report_path,
                        &entry_name,
                        "uninstall",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                    vec![],
                ));
            }

            let mp_fd = open_existing_codex_marketplace_fd(oms_config_dir, marketplace_root)?;
            let disk_aliases = match &mp_fd {
                Some(fd) => discover_marked_codex_bundle_aliases_at(fd, marketplace_root)?,
                None => BTreeSet::new(),
            };
            let had_recorded_row = recorded.is_some();
            let target_repo_path = match &recorded {
                Some(row) => row.path.clone(),
                None if disk_aliases.contains(&alias) => {
                    std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone())
                }
                None => {
                    let report_path =
                        std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
                    if want_json {
                        let report = panel_action_json(
                            app,
                            &alias,
                            &report_path,
                            &entry_name,
                            "uninstall",
                            false,
                        );
                        return Ok((0, json(&report)?, vec![]));
                    }
                    return Ok((
                        0,
                        format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                        vec![],
                    ));
                }
            };

            let launcher = resolve_oms_launcher_path()?;
            let recorded_hashes = match &recorded {
                Some(row) => {
                    let p_sha = row.plugin_sha256.as_deref().ok_or_else(|| {
                        Error::new("Invalid OMS project panel map: missing Codex plugin_sha256")
                    })?;
                    let m_sha = row.mcp_sha256.as_deref().ok_or_else(|| {
                        Error::new("Invalid OMS project panel map: missing Codex mcp_sha256")
                    })?;
                    Some((p_sha, m_sha))
                }
                None => None,
            };
            let (bundle_check, bundle_checkout_matches) = inspect_codex_bundle_for_uninstall(
                mp_fd.as_ref(),
                marketplace_root,
                &alias,
                &plugin_name,
                &launcher,
                &target_repo_path,
                recorded_hashes,
            )?;
            drop(mp_fd);

            if !had_recorded_row && matches!(bundle_check, CodexBundleUninstallCheck::Edited(_)) {
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &target_repo_path,
                        &entry_name,
                        "uninstall",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                    vec![],
                ));
            }

            let checkout_exists = std::fs::metadata(&target_repo_path).is_ok_and(|m| m.is_dir());
            let exec_dir = if checkout_exists {
                &target_repo_path
            } else {
                &parent
            };
            let codex_bin = resolve_vendor_program(app, cwd, false)?;
            let inventory = probe_codex_inventory(&codex_bin, exec_dir, stop)?;
            let market_configured = if let Some(configured_root) =
                inventory.marketplaces.get(CODEX_MARKETPLACE_NAME)
            {
                if !same_marketplace_root(configured_root, marketplace_root) {
                    return Err(Error::new(format!(
                        "an unmanaged Codex marketplace '{CODEX_MARKETPLACE_NAME}' is configured at {}; remove it first",
                        configured_root.display()
                    )));
                }
                true
            } else {
                false
            };

            let installed_plugin = inventory.plugins.get(&entry_name).filter(|p| p.installed);
            let plugin_matches_oms = bundle_checkout_matches
                && installed_plugin
                    .is_some_and(|p| p.matches_expected(marketplace_root, &plugin_name));
            let plugin_modified_outside_oms = installed_plugin.is_some() && !plugin_matches_oms;

            let mut removed_plugin = false;
            if plugin_matches_oms {
                let out = run_bounded_command(
                    app,
                    &codex_bin,
                    &["plugin", "remove", &entry_name],
                    exec_dir,
                    stop,
                )
                .map_err(VendorExecError::into_error)?;
                if out.code == Some(0) {
                    removed_plugin = true;
                } else if !is_codex_plugin_missing(&entry_name, &plugin_name, &out) {
                    return Err(vendor_failure("codex plugin", "remove", &out));
                }
            }

            let mut retained_market_plugins = Vec::new();
            let disposition = match bundle_check {
                CodexBundleUninstallCheck::Edited(edited_files) => {
                    // Leave the user-edited bundle, marketplace manifest, marketplace registration
                    // and root directory untouched; drop only the OMS ownership row.
                    UninstallDisposition::LeftModifiedCodex {
                        edited_files,
                        removed_plugin,
                    }
                }
                CodexBundleUninstallCheck::Absent
                | CodexBundleUninstallCheck::CleanEmptyDir
                | CodexBundleUninstallCheck::MatchesOms => {
                    let mut remaining_recorded = recorded_codex_aliases(&panels);
                    remaining_recorded.remove(&alias);
                    let mut remaining_disk = disk_aliases;
                    remaining_disk.remove(&alias);

                    let mut other_installed_in_market = Vec::new();
                    for p in inventory.plugins.values() {
                        if !p.installed {
                            continue;
                        }
                        if p.plugin_id == entry_name {
                            if plugin_modified_outside_oms {
                                other_installed_in_market.push(p.plugin_id.clone());
                            }
                            continue;
                        }
                        if p.plugin_id
                            .rsplit_once('@')
                            .is_some_and(|(_, market)| market == CODEX_MARKETPLACE_NAME)
                        {
                            other_installed_in_market.push(p.plugin_id.clone());
                        }
                    }

                    let mut removed_any = removed_plugin;
                    let can_remove_market_and_root = remaining_recorded.is_empty()
                        && remaining_disk.is_empty()
                        && other_installed_in_market.is_empty();
                    if remaining_recorded.is_empty() {
                        retained_market_plugins = other_installed_in_market;
                    }

                    if can_remove_market_and_root && market_configured {
                        let mp_out = run_bounded_command(
                            app,
                            &codex_bin,
                            &["plugin", "marketplace", "remove", CODEX_MARKETPLACE_NAME],
                            exec_dir,
                            stop,
                        )
                        .map_err(VendorExecError::into_error)?;
                        if mp_out.code == Some(0) {
                            removed_any = true;
                        } else if !is_codex_marketplace_missing(&mp_out) {
                            return Err(vendor_failure(
                                "codex plugin",
                                "marketplace remove",
                                &mp_out,
                            ));
                        }
                    }

                    let keep_root = !can_remove_market_and_root;
                    if update_or_delete_codex_marketplace_disk(
                        oms_config_dir,
                        marketplace_root,
                        Some(&plugin_name),
                        &remaining_disk,
                        keep_root,
                    )? {
                        removed_any = true;
                    }

                    if removed_any {
                        UninstallDisposition::Removed
                    } else {
                        UninstallDisposition::AlreadyGone
                    }
                }
            };

            if had_recorded_row {
                drop_panel_row(&mut panels, &alias, app);
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
            }

            let action = if checkout_exists {
                "uninstall"
            } else {
                "forgotten"
            };
            if want_json {
                let mut report =
                    panel_action_json(app, &alias, &target_repo_path, &entry_name, action, true);
                if let UninstallDisposition::LeftModifiedCodex { edited_files, .. } = &disposition {
                    report["preservedFiles"] = json!(edited_files);
                }
                if !retained_market_plugins.is_empty() {
                    report["retainedMarketplacePlugins"] = json!(retained_market_plugins);
                }
                return Ok((0, json(&report)?, vec![]));
            }

            let retained_suffix = if retained_market_plugins.is_empty() {
                String::new()
            } else {
                format!(
                    " (kept marketplace {CODEX_MARKETPLACE_NAME} and {} because Codex still lists installed plugin(s): {})",
                    marketplace_root.display(),
                    retained_market_plugins.join(", ")
                )
            };

            let msg = match disposition {
                UninstallDisposition::LeftModifiedCodex {
                    edited_files,
                    removed_plugin,
                } => {
                    let files_str = edited_files
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if removed_plugin {
                        format!(
                            "Codex panel bundle file(s) {files_str} were changed outside OMS; removed {entry_name} from Codex, left the bundle and marketplace in place, and dropped ownership record for {alias} ({})\n",
                            target_repo_path.display()
                        )
                    } else {
                        format!(
                            "Codex panel bundle file(s) {files_str} were changed outside OMS; left the bundle and marketplace in place and dropped ownership record for {alias} ({})\n",
                            target_repo_path.display()
                        )
                    }
                }
                _ if !checkout_exists => format!(
                    "{} panel {entry_name} for {alias} ({}): checkout is gone; cleaned up panel and dropped the record{retained_suffix}\n",
                    app.as_str(),
                    target_repo_path.display()
                ),
                UninstallDisposition::Removed => format!(
                    "uninstalled {} panel {entry_name} for {alias}: {}{retained_suffix}\n",
                    app.as_str(),
                    target_repo_path.display()
                ),
                UninstallDisposition::AlreadyGone => format!(
                    "{} panel {entry_name} for {alias} was already gone; dropped ownership record ({}){retained_suffix}\n",
                    app.as_str(),
                    target_repo_path.display()
                ),
                UninstallDisposition::LeftModifiedClaude => unreachable!(),
            };
            Ok((0, msg.into_bytes(), vec![]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeScope, CmdOutput, OMS_PACKAGE_VERSION, PanelApp, REPO_OMS_PLUGIN_JSON,
        claude_panel_entry_json, codex_mcp_manifest_value, codex_panel_resource_version,
        codex_plugin_manifest_value, codex_plugin_name, is_mcp_entry_missing, parse_claude_mcp_get,
        parse_codex_marketplace_list, parse_codex_plugin_list, parse_panels_json,
    };
    use std::path::Path;

    #[test]
    fn claude_mcp_get_parses_text_and_json_scopes() {
        let launcher = Path::new("/tmp/runtime-owner/.local/bin/oms");
        let repo = Path::new("/srv/checkouts/alpha-service");
        let local_text = format!(
            "oms-panel:\n  Scope: Local config (private to you in this project)\n  Status: Connected\n  Type: stdio\n  Command: {}\n  Args: mcp-server\n  Environment:\n    OMS_MCP_TOOL_PROFILE=chatgpt-panel\n    OMS_PANEL_REPO={}\n\nTo remove this server, run: claude mcp remove \"oms-panel\" -s local\n",
            launcher.display(),
            repo.display()
        );
        let parsed = parse_claude_mcp_get(&local_text).unwrap();
        assert_eq!(parsed.scope, Some(ClaudeScope::Local));
        assert!(parsed.matches_expected(launcher, repo));
        assert!(!parsed.matches_expected(launcher, Path::new("/srv/checkouts/other")));

        let user_text = format!(
            "oms-panel:\n  Scope: User config (available in all your projects)\n  Type: stdio\n  Command: {}\n  Args: mcp-server\n  Environment:\n    OMS_MCP_TOOL_PROFILE=mcp-app\n",
            launcher.display()
        );
        let user_parsed = parse_claude_mcp_get(&user_text).unwrap();
        assert_eq!(user_parsed.scope, Some(ClaudeScope::User));
        assert!(!user_parsed.matches_expected(launcher, repo));

        let json_local = claude_panel_entry_json(launcher, repo).unwrap();
        let wrapped = format!(r#"{{"scope":"local","config":{json_local}}}"#);
        let from_json = parse_claude_mcp_get(&wrapped).unwrap();
        assert!(from_json.matches_expected(launcher, repo));

        for unparseable in [
            "",
            "oms-panel: unexpected output without scope",
            "Scope: Local config",
            "{broken json",
            r#"{"scope":"local"}"#,
            "No MCP server found with name: oms-panel",
        ] {
            assert!(
                parse_claude_mcp_get(unparseable).is_none(),
                "unexpectedly parsed {unparseable:?}"
            );
        }

        assert!(is_mcp_entry_missing(&CmdOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "No local MCP server found with name: oms-panel".into(),
        }));
        assert!(!is_mcp_entry_missing(&CmdOutput {
            code: Some(127),
            stdout: String::new(),
            stderr: "claude: oms-panel: not found".into(),
        }));
        assert!(!is_mcp_entry_missing(&CmdOutput {
            code: Some(126),
            stdout: String::new(),
            stderr: "No local MCP server found with name: oms-panel".into(),
        }));
        assert!(!is_mcp_entry_missing(&CmdOutput {
            code: None,
            stdout: String::new(),
            stderr: "No local MCP server found with name: oms-panel".into(),
        }));
        assert!(!is_mcp_entry_missing(&CmdOutput {
            code: Some(0),
            stdout: "No MCP server found with name: oms-panel".into(),
            stderr: String::new(),
        }));
        assert!(!is_mcp_entry_missing(&CmdOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "No MCP server found with name: other-entry".into(),
        }));
    }

    #[test]
    fn codex_plugin_naming_manifest_and_inventory_parsers_fail_closed() {
        let repo_plugin: serde_json::Value = serde_json::from_str(REPO_OMS_PLUGIN_JSON).unwrap();
        assert_eq!(OMS_PACKAGE_VERSION, repo_plugin["version"]);
        assert!(!codex_panel_resource_version().is_empty());

        for ok_alias in ["a", "alpha-service", "v1-2-3-rc1", &"a".repeat(60)] {
            assert_eq!(
                codex_plugin_name(ok_alias).unwrap(),
                format!("oms-{ok_alias}")
            );
        }
        for bad_alias in [
            "",
            "Alpha",
            "alpha_service",
            "alpha.service",
            "v1.2.3-rc1",
            "alpha--service",
            "alpha..service",
            "alpha.-service",
            "alpha-",
            "alpha.",
            "-alpha",
            ".alpha",
            &"a".repeat(61),
        ] {
            let err = codex_plugin_name(bad_alias).unwrap_err();
            assert!(
                err.message.contains("valid Codex plugin name"),
                "unexpected error for {bad_alias:?}: {}",
                err.message
            );
        }

        let p_manifest = codex_plugin_manifest_value("alpha-service").unwrap();
        assert_eq!(p_manifest["name"], "oms-alpha-service");
        assert_eq!(p_manifest["version"], OMS_PACKAGE_VERSION);
        assert_eq!(
            p_manifest["extensions"]["com.openai"]["interface"]["displayName"],
            "OMS · alpha-service"
        );

        let mcp_manifest = codex_mcp_manifest_value(
            Path::new("/tmp/runtime-owner/.local/bin/oms"),
            Path::new("/srv/checkouts/alpha-service"),
        )
        .unwrap();
        let srv = &mcp_manifest["mcpServers"]["oms-panel"];
        assert_eq!(srv["type"], "stdio");
        assert_eq!(srv["command"], "/tmp/runtime-owner/.local/bin/oms");
        assert_eq!(srv["args"], serde_json::json!(["mcp-server"]));
        assert_eq!(srv["env"]["OMS_MCP_TOOL_PROFILE"], "chatgpt-panel");
        assert_eq!(srv["env"]["OMS_PANEL_REPO"], "/srv/checkouts/alpha-service");
        assert_eq!(
            srv["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"],
            codex_panel_resource_version()
        );

        let valid_list = r#"{"installed":[{"pluginId":"oms-alpha@oms-host-panels","name":"oms-alpha","marketplaceName":"oms-host-panels","installed":true,"enabled":true,"version":"0.7.0-older","source":{"source":"local","path":"/tmp/cfg/oh-my-setting/codex-panels/plugins/oms-alpha"}}],"available":[]}"#;
        let plugins = parse_codex_plugin_list(valid_list).unwrap();
        let mp_root = Path::new("/tmp/cfg/oh-my-setting/codex-panels");
        assert!(plugins["oms-alpha@oms-host-panels"].installed);
        assert!(plugins["oms-alpha@oms-host-panels"].matches_expected(mp_root, "oms-alpha"));
        let local_ver_list = r#"{"installed":[{"pluginId":"oms-alpha@oms-host-panels","installed":true,"enabled":true,"version":"local"}],"available":[]}"#;
        let local_plugins = parse_codex_plugin_list(local_ver_list).unwrap();
        assert!(local_plugins["oms-alpha@oms-host-panels"].matches_expected(mp_root, "oms-alpha"));
        let foreign_src_list = r#"{"installed":[{"pluginId":"oms-alpha@oms-host-panels","installed":true,"enabled":true,"source":{"source":"local","path":"/tmp/foreign/oms-alpha"}}],"available":[]}"#;
        let foreign_plugins = parse_codex_plugin_list(foreign_src_list).unwrap();
        assert!(
            !foreign_plugins["oms-alpha@oms-host-panels"].matches_expected(mp_root, "oms-alpha")
        );

        let launcher = Path::new("/tmp/runtime-owner/.local/bin/oms");
        let repo = Path::new("/srv/checkouts/alpha-service");
        let old_p = super::encode_pretty_json(
            &super::codex_plugin_manifest_value_for_version("alpha-service", "0.7.0").unwrap(),
        )
        .unwrap();
        let old_m = super::encode_pretty_json(
            &super::codex_mcp_manifest_value_for_version(launcher, repo, "panel-res-v0").unwrap(),
        )
        .unwrap();
        assert!(
            super::codex_bundle_bytes_match_any_oms_release(
                "alpha-service",
                launcher,
                repo,
                &old_p,
                &old_m,
            )
            .unwrap()
        );
        assert!(
            !super::codex_bundle_bytes_match_any_oms_release(
                "beta-service",
                launcher,
                repo,
                &old_p,
                &old_m,
            )
            .unwrap()
        );
        assert!(
            !super::codex_bundle_bytes_match_any_oms_release(
                "alpha-service",
                launcher,
                Path::new("/srv/checkouts/other"),
                &old_p,
                &old_m,
            )
            .unwrap()
        );
        for bad_list in [
            "",
            "{}",
            "{broken",
            r#"{"installed":"nope"}"#,
            r#"{"installed":[{"pluginId":"","installed":true}]}"#,
            r#"{"installed":[{"pluginId":"oms-alpha@oms-host-panels"}]}"#,
            r#"{"installed":[{"pluginId":"oms-alpha@oms-host-panels","installed":true,"version":123}]}"#,
        ] {
            assert!(
                parse_codex_plugin_list(bad_list).is_none(),
                "accepted {bad_list:?}"
            );
        }

        assert!(parse_codex_marketplace_list("").unwrap().is_empty());
        assert!(
            parse_codex_marketplace_list("No plugin marketplaces in scope.\n")
                .unwrap()
                .is_empty()
        );
        let table = "MARKETPLACE          ROOT\noms-host-panels      /tmp/cfg/oh-my-setting/codex-panels\noh-my-setting-local  /srv/oms\n";
        let mps = parse_codex_marketplace_list(table).unwrap();
        assert_eq!(
            mps["oms-host-panels"],
            Path::new("/tmp/cfg/oh-my-setting/codex-panels")
        );
        for bad_mp in [
            "unexpected output without root path",
            "Error: failed to load",
            "oms-host-panels relative/path",
            "MARKETPLACE ONLY",
        ] {
            assert!(
                parse_codex_marketplace_list(bad_mp).is_none(),
                "accepted {bad_mp:?}"
            );
        }
    }

    #[test]
    fn panel_app_and_sidecar_validation_stay_strict() {
        assert_eq!(
            oms_scrub::scan(include_bytes!("project_panels.rs"), &[]),
            Ok(Vec::new())
        );
        assert_eq!(PanelApp::parse("claude").unwrap(), PanelApp::Claude);
        assert_eq!(PanelApp::parse("codex").unwrap(), PanelApp::Codex);
        let err = PanelApp::parse("antigravity").unwrap_err();
        assert!(
            err.message.contains("supported: claude, codex"),
            "{}",
            err.message
        );

        let sha_a = "a".repeat(64);
        let sha_b = "b".repeat(64);
        let ok = format!(
            r#"{{"alpha":[{{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"}},{{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha","plugin_sha256":"{sha_a}","mcp_sha256":"{sha_b}"}}]}}"#
        );
        assert_eq!(parse_panels_json(&ok).unwrap()["alpha"].len(), 2);

        let claude_with_hashes = format!(
            r#"{{"alpha":[{{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha","plugin_sha256":"{sha_a}","mcp_sha256":"{sha_b}"}}]}}"#
        );
        let codex_one_hash = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha","plugin_sha256":"{sha_a}"}}]}}"#
        );
        let codex_upper_hash = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha","plugin_sha256":"{}","mcp_sha256":"{sha_b}"}}]}}"#,
            "A".repeat(64)
        );
        for bad in [
            r#"{"alpha":[]}"#,
            r#"{"alpha":[{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha"}]}"#,
            claude_with_hashes.as_str(),
            codex_one_hash.as_str(),
            codex_upper_hash.as_str(),
            r#"{"alpha":[{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha","plugin_sha256":"short","mcp_sha256":"short"}]}"#,
            r#"{"alpha":[{"app":"codex","scope":"local","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"codex","scope":"user","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"codex","scope":"user","name":"oms-beta@oms-host-panels","path":"/srv/alpha"}]}"#,
            r#"{"Alpha":[{"app":"codex","scope":"user","name":"oms-Alpha@oms-host-panels","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"user","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oh-my-setting","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"relative/alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/../alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha","extra":1}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"},{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"}],"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"}]}"#,
        ] {
            assert!(parse_panels_json(bad).is_err(), "accepted {bad}");
        }
    }
}
