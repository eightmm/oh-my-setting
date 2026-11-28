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
const OMS_PACKAGE_VERSION: &str = include_str!("../../../VERSION").trim_ascii();
const REPO_OMS_MCP_JSON: &str = include_str!("../../../plugins/oms/mcp.json");
const CODEX_BLOCK_BEGIN: &str = "# BEGIN oh-my-setting codex-panel";
const CODEX_BLOCK_END: &str = "# END oh-my-setting codex-panel";
const CODEX_UNTRUSTED_REASON: &str = "codex-project-untrusted";
const CODEX_TRUST_UNKNOWN_REASON: &str = "codex-trust-unknown";
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const VENDOR_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const MCP_MISSING_STATUS: i32 = 1;
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PanelApp {
    Claude,
    Codex,
    Antigravity,
}

impl PanelApp {
    pub(crate) const SUPPORTED: &'static str = "claude, codex, antigravity";

    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "antigravity" => Ok(Self::Antigravity),
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
            Self::Antigravity => "antigravity",
        }
    }

    const fn expected_scope(self) -> &'static str {
        match self {
            Self::Claude => "local",
            Self::Codex | Self::Antigravity => "project",
        }
    }
}

fn codex_panel_resource_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let mcp_val: serde_json::Value =
            serde_json::from_str(REPO_OMS_MCP_JSON).expect("compiled plugins/oms/mcp.json");
        mcp_val["mcpServers"]["oms-panel"]["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"]
            .as_str()
            .expect("OMS_MCP_PANEL_RESOURCE_VERSION in plugins/oms/mcp.json")
            .to_owned()
    })
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PanelRecord {
    pub app: String,
    pub scope: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_config: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_codex_dir: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_agents_dir: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_exclude: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator_added: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_separator_added: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_mcp_servers: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexRowMeta {
    block_sha256: String,
    created_config: bool,
    created_codex_dir: bool,
    created_exclude: bool,
    separator_added: bool,
    exclude_separator_added: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AntigravityRowMeta {
    entry_sha256: String,
    created_config: bool,
    created_agents_dir: bool,
    created_exclude: bool,
    exclude_separator_added: bool,
    created_mcp_servers: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowMeta {
    Claude,
    Codex(CodexRowMeta),
    Antigravity(AntigravityRowMeta),
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

fn validate_panel_map(panels: &PanelMap) -> Result<()> {
    require(
        panels.len() <= MAX_ENTRIES,
        "Invalid OMS project panel map: exceeds 128 entries",
    )?;
    let mut seen_codex_paths = BTreeSet::new();
    let mut seen_antigravity_paths = BTreeSet::new();
    for (alias, rows) in panels {
        require(
            valid_alias(alias) && !rows.is_empty() && rows.len() <= 8,
            "Invalid OMS project panel map: expected valid aliases with nonempty panel lists",
        )?;
        let mut seen = BTreeSet::new();
        for row in rows {
            let app = PanelApp::parse(&row.app).map_err(|_| {
                Error::new("Invalid OMS project panel map: unsupported panel app in record")
            })?;
            let fields_valid = match app {
                PanelApp::Claude => {
                    row.block_sha256.is_none()
                        && row.entry_sha256.is_none()
                        && row.created_config.is_none()
                        && row.created_codex_dir.is_none()
                        && row.created_agents_dir.is_none()
                        && row.created_exclude.is_none()
                        && row.separator_added.is_none()
                        && row.exclude_separator_added.is_none()
                        && row.created_mcp_servers.is_none()
                }
                PanelApp::Codex => {
                    row.block_sha256.as_deref().is_some_and(valid_sha256_hex)
                        && row.entry_sha256.is_none()
                        && row.created_config.is_some()
                        && row.created_codex_dir.is_some()
                        && row.created_agents_dir.is_none()
                        && row.created_exclude.is_some()
                        && row.created_mcp_servers.is_none()
                        && !(row.created_codex_dir == Some(true)
                            && row.created_config == Some(false))
                        && !(row.exclude_separator_added == Some(true)
                            && row.created_exclude != Some(true))
                        && seen_codex_paths.insert(row.path.clone())
                }
                PanelApp::Antigravity => {
                    row.entry_sha256.as_deref().is_some_and(valid_sha256_hex)
                        && row.block_sha256.is_none()
                        && row.created_config.is_some()
                        && row.created_agents_dir.is_some()
                        && row.created_codex_dir.is_none()
                        && row.created_exclude.is_some()
                        && row.separator_added.is_none()
                        && !(row.created_agents_dir == Some(true)
                            && row.created_config == Some(false))
                        && !(row.exclude_separator_added == Some(true)
                            && row.created_exclude != Some(true))
                        && row.exclude_separator_added != Some(false)
                        && row.created_mcp_servers != Some(false)
                        && !(row.created_mcp_servers == Some(true)
                            && row.created_config == Some(true))
                        && seen_antigravity_paths.insert(row.path.clone())
                }
            };
            require(
                row.scope == app.expected_scope()
                    && row.name == PANEL_ENTRY_NAME
                    && row.name != LIFECYCLE_OWNED_KEY
                    && normalized_abs_utf8(&row.path)
                    && fields_valid
                    && seen.insert(app),
                "Invalid OMS project panel map: invalid or duplicate panel record fields",
            )?;
        }
    }
    Ok(())
}

fn parse_panels_json(raw: &str) -> Result<PanelMap> {
    require(
        raw.len() <= MAX_BYTES,
        "Invalid OMS project panel map: exceeds 16 KiB",
    )?;
    let StrictPanelMap(panels) = serde_json::from_str(raw).map_err(|_| {
        Error::new(
            "Invalid OMS project panel map: expected an object of project aliases and strict panel records",
        )
    })?;
    validate_panel_map(&panels)?;
    Ok(panels)
}

fn encode_panels_json(panels: &PanelMap) -> Result<Vec<u8>> {
    require(
        !panels.is_empty() && panels.len() <= MAX_ENTRIES,
        "desktop project panel map must contain 1 to 128 entries",
    )?;
    validate_panel_map(panels)?;
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

fn toml_basic_string(raw: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                use std::fmt::Write;
                let _ = write!(&mut out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

fn codex_panel_block_for_versions(
    launcher: &Path,
    physical_repo: &Path,
    package_version: &str,
    resource_version: &str,
) -> Result<String> {
    require(
        !package_version.is_empty()
            && !package_version.chars().any(|c| c.is_control() && c != '\t'),
        "invalid OMS package version",
    )?;
    let launcher_str = launcher
        .to_str()
        .ok_or_else(|| Error::new("launcher path must be valid UTF-8"))?;
    let repo_str = physical_repo
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    Ok(format!(
        "{CODEX_BLOCK_BEGIN}\n# oh-my-setting {package_version}\n[mcp_servers.{PANEL_ENTRY_NAME}]\ncommand = {}\nargs = [\"mcp-server\"]\n\n[mcp_servers.{PANEL_ENTRY_NAME}.env]\nOMS_MCP_TOOL_PROFILE = \"chatgpt-panel\"\nOMS_PANEL_REPO = {}\nOMS_MCP_PANEL_RESOURCE_VERSION = {}\n{CODEX_BLOCK_END}\n",
        toml_basic_string(launcher_str)?,
        toml_basic_string(repo_str)?,
        toml_basic_string(resource_version)?,
    ))
}

fn codex_panel_block(launcher: &Path, physical_repo: &Path) -> Result<String> {
    codex_panel_block_for_versions(
        launcher,
        physical_repo,
        OMS_PACKAGE_VERSION,
        codex_panel_resource_version(),
    )
}

fn expected_codex_panel_server_table(
    launcher: &Path,
    physical_repo: &Path,
    resource_version: &str,
) -> Result<oms_toml::Table> {
    let launcher_str = launcher
        .to_str()
        .ok_or_else(|| Error::new("launcher path must be valid UTF-8"))?;
    let repo_str = physical_repo
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let mut env_table = oms_toml::Table::new();
    env_table.insert(
        "OMS_MCP_TOOL_PROFILE".into(),
        oms_toml::Value::String("chatgpt-panel".into()),
    );
    env_table.insert(
        "OMS_PANEL_REPO".into(),
        oms_toml::Value::String(repo_str.to_owned()),
    );
    env_table.insert(
        "OMS_MCP_PANEL_RESOURCE_VERSION".into(),
        oms_toml::Value::String(resource_version.to_owned()),
    );

    let mut server_table = oms_toml::Table::new();
    server_table.insert(
        "command".into(),
        oms_toml::Value::String(launcher_str.to_owned()),
    );
    server_table.insert(
        "args".into(),
        oms_toml::Value::Array(vec![oms_toml::Value::String("mcp-server".into())]),
    );
    server_table.insert("env".into(), oms_toml::Value::Table(env_table));
    Ok(server_table)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockSplit<'a> {
    before: &'a str,
    block: Option<&'a str>,
    after: &'a str,
}

fn split_codex_panel_block(raw: &str) -> Result<BlockSplit<'_>> {
    let mut pos = 0usize;
    let mut state: Option<(usize, Option<usize>)> = None;
    while pos < raw.len() {
        let end = match raw[pos..].find('\n') {
            Some(rel) => pos + rel + 1,
            None => raw.len(),
        };
        let line = &raw[pos..end];
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        let trimmed = stripped.trim();
        if trimmed == CODEX_BLOCK_BEGIN {
            match state {
                None => state = Some((pos, None)),
                Some(_) => {
                    return Err(Error::new(
                        "malformed OMS codex-panel block in .codex/config.toml: duplicate begin marker",
                    ));
                }
            }
        } else if trimmed == CODEX_BLOCK_END {
            match state {
                Some((start, None)) => state = Some((start, Some(end))),
                None => {
                    return Err(Error::new(
                        "malformed OMS codex-panel block in .codex/config.toml: unmatched end marker",
                    ));
                }
                Some((_, Some(_))) => {
                    return Err(Error::new(
                        "malformed OMS codex-panel block in .codex/config.toml: duplicate end marker",
                    ));
                }
            }
        }
        pos = end;
    }
    match state {
        None => Ok(BlockSplit {
            before: raw,
            block: None,
            after: "",
        }),
        Some((_, None)) => Err(Error::new(
            "malformed OMS codex-panel block in .codex/config.toml: missing end marker",
        )),
        Some((start, Some(end))) => Ok(BlockSplit {
            before: &raw[..start],
            block: Some(&raw[start..end]),
            after: &raw[end..],
        }),
    }
}

fn table_without_oms_panel(
    mut table: oms_toml::Table,
    keep_empty_mcp_servers: bool,
) -> Result<oms_toml::Table> {
    let remove_mcp_servers = match table.get_mut("mcp_servers") {
        None => false,
        Some(oms_toml::Value::Table(servers)) => {
            servers.remove(PANEL_ENTRY_NAME);
            servers.is_empty() && !keep_empty_mcp_servers
        }
        Some(_) => {
            return Err(Error::new(
                "Invalid .codex/config.toml: mcp_servers must be a TOML table",
            ));
        }
    };
    if remove_mcp_servers {
        table.remove("mcp_servers");
    }
    Ok(table)
}

fn table_with_expected_oms_panel(
    mut base: oms_toml::Table,
    launcher: &Path,
    physical_repo: &Path,
    resource_version: &str,
) -> Result<oms_toml::Table> {
    let server = expected_codex_panel_server_table(launcher, physical_repo, resource_version)?;
    let entry = base
        .entry("mcp_servers".into())
        .or_insert_with(|| oms_toml::Value::Table(oms_toml::Table::new()));
    let oms_toml::Value::Table(servers) = entry else {
        return Err(Error::new(
            "Invalid .codex/config.toml: mcp_servers must be a TOML table",
        ));
    };
    servers.insert(PANEL_ENTRY_NAME.into(), oms_toml::Value::Table(server));
    Ok(base)
}

fn expected_antigravity_panel_entry_for_version(
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
    let mut env_map = serde_json::Map::new();
    env_map.insert("OMS_MCP_TOOL_PROFILE".into(), json!("chatgpt-panel"));
    env_map.insert("OMS_PANEL_REPO".into(), json!(repo_str));
    env_map.insert(
        "OMS_MCP_PANEL_RESOURCE_VERSION".into(),
        json!(resource_version),
    );
    let mut entry = serde_json::Map::new();
    entry.insert("command".into(), json!(launcher_str));
    entry.insert("args".into(), json!(["mcp-server"]));
    entry.insert("env".into(), serde_json::Value::Object(env_map));
    Ok(serde_json::Value::Object(entry))
}

fn expected_antigravity_panel_entry(
    launcher: &Path,
    physical_repo: &Path,
) -> Result<serde_json::Value> {
    expected_antigravity_panel_entry_for_version(
        launcher,
        physical_repo,
        codex_panel_resource_version(),
    )
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json_value).collect())
        }
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<&str, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.as_str(), canonicalize_json_value(v)))
                .collect();
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                out.insert(k.to_owned(), v);
            }
            serde_json::Value::Object(out)
        }
        other => other.clone(),
    }
}

fn canonical_json_sha256(value: &serde_json::Value) -> Result<String> {
    let canonical = canonicalize_json_value(value);
    let bytes = serde_json::to_vec(&canonical).map_err(Error::new)?;
    Ok(oms_panel_core::digest(&bytes))
}

fn parse_strict_json_object_bounded(
    raw: &[u8],
    label: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    require(
        raw.len() <= MAX_CONFIG_BYTES,
        &format!("Invalid {label}: exceeds 1 MiB"),
    )?;
    let _ = std::str::from_utf8(raw)
        .map_err(|_| Error::new(format!("Invalid {label}: file is not valid UTF-8")))?;
    let opts = oms_json::ParseOptions {
        max_bytes: MAX_CONFIG_BYTES,
        max_depth: 128,
        ..oms_json::ParseOptions::strict()
    };
    oms_json::parse_with_options(raw, &opts)
        .map_err(|e| Error::new(format!("Invalid {label}: {e}")))?;
    let parsed: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| Error::new(format!("Invalid {label}: {e}")))?;
    match parsed {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err(Error::new(format!(
            "Invalid {label}: root value must be a JSON object"
        ))),
    }
}

fn encode_pretty_2space_json(map: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(map).map_err(Error::new)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn open_dir_nofollow(path: &Path, label: &str) -> Result<OwnedFd> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::new(format!("refusing symlinked {label}")));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(Error::new(format!("Invalid {label}: not a directory")));
        }
        Ok(_) => {}
        Err(e) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    }
    match fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(fd),
        Err(Errno::LOOP) => Err(Error::new(format!("refusing symlinked {label}"))),
        Err(e) => Err(Error::new(format!("cannot open {label}: {e}"))),
    }
}

fn open_existing_subdir_nofollow_at(
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
            require(
                stat.st_uid == rustix::process::geteuid().as_raw(),
                &format!("Invalid {label}: directory is not owned by the current user"),
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
    require(
        FileType::from_raw_mode(stat.st_mode) == FileType::Directory,
        &format!("Invalid {label}: not a directory"),
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {label}: directory is not owned by the current user"),
    )?;
    Ok(Some(fd))
}

fn ensure_subdir_nofollow_at(
    parent_fd: &OwnedFd,
    name: &OsStr,
    create_mode: u32,
    label: &str,
) -> Result<(OwnedFd, bool)> {
    let created = match fs::mkdirat(parent_fd, name, Mode::from_raw_mode(create_mode)) {
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
        fs::fchmod(&fd, Mode::from_raw_mode(create_mode))
            .map_err(|e| Error::new(format!("cannot set permissions on {label}: {e}")))?;
        sync_dir(parent_fd)?;
    }
    let stat = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot stat {label}: {e}")))?;
    require(
        FileType::from_raw_mode(stat.st_mode) == FileType::Directory,
        &format!("Invalid {label}: not a directory"),
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {label}: directory is not owned by the current user"),
    )?;
    Ok((fd, created))
}

fn validate_regular_file_stat(stat: &Stat, label: &str) -> Result<()> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(Error::new(format!("refusing symlinked {label}")));
    }
    require(
        kind == FileType::RegularFile,
        &format!("Invalid {label}: not a regular file"),
    )?;
    require(
        stat.st_nlink == 1,
        &format!("Invalid {label}: file must not have hard links"),
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {label}: file is not owned by the current user"),
    )?;
    Ok(())
}

fn read_regular_file_nofollow_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
    label: &str,
) -> Result<Option<(Vec<u8>, Stat)>> {
    let named = match fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            validate_regular_file_stat(&stat, label)?;
            stat
        }
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    };
    let fd = match fs::openat(
        dir_fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new(format!("refusing symlinked {label}"))),
        Err(e) => return Err(Error::new(format!("cannot open {label}: {e}"))),
    };
    let opened = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot inspect {label}: {e}")))?;
    validate_regular_file_stat(&opened, label)?;
    require(
        same_stat(&named, &opened),
        &format!("{label} changed while opening"),
    )?;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(format!("cannot read {label}: {e}")))?;
    require(
        bytes.len() <= MAX_CONFIG_BYTES,
        &format!("Invalid {label}: exceeds 1 MiB"),
    )?;
    let after = fs::fstat(&file).map_err(|e| Error::new(format!("cannot verify {label}: {e}")))?;
    require(
        same_stat(&opened, &after),
        &format!("{label} changed while reading"),
    )?;
    Ok(Some((bytes, after)))
}

fn write_regular_file_atomic_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
    before: Option<&Stat>,
    bytes: &[u8],
    mode: u32,
    label: &str,
) -> Result<Stat> {
    let clean_mode = mode & 0o777;
    let mut temp = TempMapFile::create(dir_fd, name)?;
    temp.file
        .write_all(bytes)
        .map_err(|e| Error::new(format!("cannot write {label}: {e}")))?;
    fs::fchmod(&temp.file, Mode::from_raw_mode(clean_mode))
        .map_err(|e| Error::new(format!("cannot set permissions on {label}: {e}")))?;
    fs::fsync(&temp.file).map_err(|e| Error::new(format!("cannot sync {label}: {e}")))?;
    let staged =
        fs::fstat(&temp.file).map_err(|e| Error::new(format!("cannot verify {label}: {e}")))?;
    validate_regular_file_stat(&staged, label)?;
    require(
        staged.st_mode & 0o7777 == clean_mode,
        &format!("staged {label} did not retain expected permissions"),
    )?;
    require(
        temp.owns_entry(),
        &format!("staged {label} changed before publication"),
    )?;
    match (before, fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW)) {
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
            validate_regular_file_stat(&curr, label)?;
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
    sync_dir(dir_fd)?;
    fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| Error::new(format!("cannot stat published {label}: {e}")))
}

fn unlink_regular_file_checked_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
    before: Option<&Stat>,
    label: &str,
) -> Result<()> {
    match (before, fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW)) {
        (_, Err(Errno::NOENT)) => return Ok(()),
        (_, Ok(stat)) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            return Err(Error::new(format!("refusing symlinked {label}")));
        }
        (Some(prev), Ok(curr)) => {
            validate_regular_file_stat(&curr, label)?;
            require(
                same_stat(prev, &curr),
                &format!("{label} changed concurrently; retry"),
            )?;
        }
        (None, Ok(curr)) => {
            validate_regular_file_stat(&curr, label)?;
        }
        (_, Err(e)) => return Err(Error::new(format!("cannot inspect {label}: {e}"))),
    }
    match fs::unlinkat(dir_fd, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => sync_dir(dir_fd),
        Err(e) => Err(Error::new(format!("cannot remove {label}: {e}"))),
    }
}

fn try_rmdir_if_empty_at(parent_fd: &OwnedFd, name: &OsStr, label: &str) -> Result<bool> {
    match fs::unlinkat(parent_fd, name, AtFlags::REMOVEDIR) {
        Ok(()) => {
            sync_dir(parent_fd)?;
            Ok(true)
        }
        Err(Errno::NOENT | Errno::NOTEMPTY | Errno::EXIST) => Ok(false),
        Err(e) => Err(Error::new(format!("cannot remove {label}: {e}"))),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CodexTrustStatus {
    Trusted,
    Untrusted { trust_target: PathBuf },
    Unknown { hint: String },
}

fn sanitize_single_line(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn resolve_codex_home_dir_with_env(
    cwd: &Path,
    codex_home_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> std::result::Result<PathBuf, String> {
    if let Some(raw) = codex_home_env.filter(|v| !v.is_empty()) {
        if raw.to_str().is_none() || raw.as_bytes().contains(&0) {
            return Err("Cannot verify Codex project trust: CODEX_HOME is not valid UTF-8.".into());
        }
        let candidate = PathBuf::from(raw);
        return Ok(if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        });
    }
    let Some(home) = home_env.filter(|v| !v.is_empty()) else {
        return Err(
            "Cannot verify Codex project trust: neither CODEX_HOME nor HOME is set.".into(),
        );
    };
    let home_path = PathBuf::from(home);
    if !normalized_abs_utf8(&home_path) {
        return Err(
            "Cannot verify Codex project trust: HOME is not a normalized UTF-8 absolute path."
                .into(),
        );
    }
    Ok(home_path.join(".codex"))
}

fn resolve_codex_trust_in_toml(
    raw: &[u8],
    physical_repo: &Path,
    repo_root: Option<&Path>,
    config_label: &Path,
) -> CodexTrustStatus {
    let trust_target = repo_root.unwrap_or(physical_repo).to_path_buf();
    let Some(cwd_str) = physical_repo.to_str() else {
        return CodexTrustStatus::Untrusted { trust_target };
    };
    let Ok(table) = oms_toml::parse_bounded(raw, MAX_CONFIG_BYTES) else {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} is not valid TOML.",
                config_label.display()
            )),
        };
    };
    let projects = match table.get("projects") {
        None => return CodexTrustStatus::Untrusted { trust_target },
        Some(oms_toml::Value::Table(projects)) => projects,
        Some(_) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: {} has an invalid 'projects' entry.",
                    config_label.display()
                )),
            };
        }
    };

    match projects.get(cwd_str) {
        Some(oms_toml::Value::Table(cwd_proj)) => {
            if let Some(level) = cwd_proj.get("trust_level") {
                return if level.as_str() == Some("trusted") {
                    CodexTrustStatus::Trusted
                } else {
                    CodexTrustStatus::Untrusted {
                        trust_target: physical_repo.to_path_buf(),
                    }
                };
            }
        }
        Some(_) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: {} has an invalid entry under [projects].",
                    config_label.display()
                )),
            };
        }
        None => {}
    }

    if let Some(root) = repo_root
        && root != physical_repo
        && let Some(root_str) = root.to_str()
    {
        match projects.get(root_str) {
            Some(oms_toml::Value::Table(root_proj)) => {
                if root_proj
                    .get("trust_level")
                    .and_then(oms_toml::Value::as_str)
                    == Some("trusted")
                {
                    return CodexTrustStatus::Trusted;
                }
            }
            Some(_) => {
                return CodexTrustStatus::Unknown {
                    hint: sanitize_single_line(&format!(
                        "Cannot verify Codex project trust: {} has an invalid entry under [projects].",
                        config_label.display()
                    )),
                };
            }
            None => {}
        }
    }

    CodexTrustStatus::Untrusted { trust_target }
}

fn check_codex_project_trust_at(
    codex_home: &Path,
    physical_repo: &Path,
    repo_root: Option<&Path>,
    expected_uid: u32,
) -> CodexTrustStatus {
    use std::os::unix::fs::MetadataExt;
    let trust_target = repo_root.unwrap_or(physical_repo).to_path_buf();

    match std::fs::symlink_metadata(codex_home) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return CodexTrustStatus::Untrusted { trust_target };
        }
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot inspect CODEX_HOME ({}): {e}.",
                    codex_home.display()
                )),
            };
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: CODEX_HOME ({}) is a symlink.",
                    codex_home.display()
                )),
            };
        }
        Ok(meta) if !meta.is_dir() => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: CODEX_HOME ({}) is not a directory.",
                    codex_home.display()
                )),
            };
        }
        Ok(meta) if meta.uid() != expected_uid => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: CODEX_HOME ({}) is not owned by the current user.",
                    codex_home.display()
                )),
            };
        }
        Ok(_) => {}
    }

    let dir_fd = match fs::open(
        codex_home,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return CodexTrustStatus::Untrusted { trust_target },
        Err(Errno::LOOP) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: CODEX_HOME ({}) is a symlink.",
                    codex_home.display()
                )),
            };
        }
        Err(Errno::NOTDIR) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: CODEX_HOME ({}) is not a directory.",
                    codex_home.display()
                )),
            };
        }
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot read CODEX_HOME ({}): {e}.",
                    codex_home.display()
                )),
            };
        }
    };

    let dir_stat = match fs::fstat(&dir_fd) {
        Ok(stat) => stat,
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot inspect CODEX_HOME ({}): {e}.",
                    codex_home.display()
                )),
            };
        }
    };
    if FileType::from_raw_mode(dir_stat.st_mode) != FileType::Directory {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: CODEX_HOME ({}) is not a directory.",
                codex_home.display()
            )),
        };
    }
    if dir_stat.st_uid != expected_uid {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: CODEX_HOME ({}) is not owned by the current user.",
                codex_home.display()
            )),
        };
    }

    let config_path = codex_home.join("config.toml");
    let named = match fs::statat(
        &dir_fd,
        OsStr::new("config.toml"),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Err(Errno::NOENT) => return CodexTrustStatus::Untrusted { trust_target },
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot inspect {}: {e}.",
                    config_path.display()
                )),
            };
        }
        Ok(stat) => {
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind == FileType::Symlink {
                return CodexTrustStatus::Unknown {
                    hint: sanitize_single_line(&format!(
                        "Cannot verify Codex project trust: {} is a symlink.",
                        config_path.display()
                    )),
                };
            }
            if kind != FileType::RegularFile {
                return CodexTrustStatus::Unknown {
                    hint: sanitize_single_line(&format!(
                        "Cannot verify Codex project trust: {} is not a regular file.",
                        config_path.display()
                    )),
                };
            }
            if stat.st_uid != expected_uid {
                return CodexTrustStatus::Unknown {
                    hint: sanitize_single_line(&format!(
                        "Cannot verify Codex project trust: {} is not owned by the current user.",
                        config_path.display()
                    )),
                };
            }
            stat
        }
    };

    let fd = match fs::openat(
        &dir_fd,
        OsStr::new("config.toml"),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return CodexTrustStatus::Untrusted { trust_target },
        Err(Errno::LOOP) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: {} is a symlink.",
                    config_path.display()
                )),
            };
        }
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot read {}: {e}.",
                    config_path.display()
                )),
            };
        }
    };

    let opened = match fs::fstat(&fd) {
        Ok(stat) => stat,
        Err(e) => {
            return CodexTrustStatus::Unknown {
                hint: sanitize_single_line(&format!(
                    "Cannot verify Codex project trust: cannot inspect {}: {e}.",
                    config_path.display()
                )),
            };
        }
    };
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} is not a regular file.",
                config_path.display()
            )),
        };
    }
    if opened.st_uid != expected_uid {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} is not owned by the current user.",
                config_path.display()
            )),
        };
    }
    if !same_stat(&named, &opened) {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} changed while opening.",
                config_path.display()
            )),
        };
    }

    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    if let Err(e) = (&mut file)
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
    {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: cannot read {}: {e}.",
                config_path.display()
            )),
        };
    }
    if bytes.len() > MAX_CONFIG_BYTES {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} exceeds 1 MiB.",
                config_path.display()
            )),
        };
    }
    let Ok(after) = fs::fstat(&file) else {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: cannot verify {} after reading.",
                config_path.display()
            )),
        };
    };
    if !same_stat(&opened, &after) {
        return CodexTrustStatus::Unknown {
            hint: sanitize_single_line(&format!(
                "Cannot verify Codex project trust: {} changed while reading.",
                config_path.display()
            )),
        };
    }

    resolve_codex_trust_in_toml(&bytes, physical_repo, repo_root, &config_path)
}

fn check_codex_project_trust_with_env(
    cwd: &Path,
    physical_repo: &Path,
    repo_root: Option<&Path>,
    codex_home_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> CodexTrustStatus {
    let codex_home = match resolve_codex_home_dir_with_env(cwd, codex_home_env, home_env) {
        Ok(dir) => dir,
        Err(hint) => return CodexTrustStatus::Unknown { hint },
    };
    check_codex_project_trust_at(
        &codex_home,
        physical_repo,
        repo_root,
        rustix::process::geteuid().as_raw(),
    )
}

fn check_codex_project_trust(
    cwd: &Path,
    physical_repo: &Path,
    repo_root: Option<&Path>,
) -> CodexTrustStatus {
    check_codex_project_trust_with_env(
        cwd,
        physical_repo,
        repo_root,
        std::env::var_os("CODEX_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn codex_untrusted_hint(trust_target: &Path) -> String {
    format!(
        "Trust {} in Codex so it loads .codex/config.toml.",
        trust_target.display()
    )
}

fn attach_codex_trust_fields(report: &mut serde_json::Value, trust: &CodexTrustStatus) {
    match trust {
        CodexTrustStatus::Trusted => {
            report["active"] = json!(true);
        }
        CodexTrustStatus::Untrusted { trust_target } => {
            report["active"] = json!(false);
            report["reason"] = json!(CODEX_UNTRUSTED_REASON);
            report["hint"] = json!(codex_untrusted_hint(trust_target));
        }
        CodexTrustStatus::Unknown { hint } => {
            report["active"] = json!(false);
            report["reason"] = json!(CODEX_TRUST_UNKNOWN_REASON);
            report["hint"] = json!(hint);
        }
    }
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

fn resolve_claude_program(cwd: &Path, dry_run: bool) -> Result<PathBuf> {
    let env_var = "OMS_CLAUDE_BIN";
    let default_bin = "claude";
    let label = "Claude CLI";
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

fn claude_timeout() -> Duration {
    if let Some(raw) = std::env::var_os("OMS_CLAUDE_TIMEOUT_MS")
        && let Some(s) = raw.to_str()
        && let Ok(ms) = s.trim().parse::<u64>()
        && (20..=300_000).contains(&ms)
    {
        return Duration::from_millis(ms);
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

fn run_bounded_process(
    label: &'static str,
    program: &Path,
    args: &[&str],
    workdir: &Path,
    clear_envs: &[&str],
    timeout: Duration,
    stop: &AtomicBool,
) -> std::result::Result<CmdOutput, VendorExecError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for key in clear_envs {
        command.env_remove(key);
    }
    let mut child = command
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

fn run_claude_command(
    program: &Path,
    args: &[&str],
    workdir: &Path,
    stop: &AtomicBool,
) -> std::result::Result<CmdOutput, VendorExecError> {
    run_bounded_process(
        "Claude CLI",
        program,
        args,
        workdir,
        &[],
        claude_timeout(),
        stop,
    )
}

enum GitConfigState {
    NotGit,
    Tracked,
    Untracked {
        common_git_dir: PathBuf,
        exclude_line: String,
        already_ignored: bool,
    },
}

fn has_any_ancestor_git_marker(path: &Path) -> bool {
    let mut cur = Some(path);
    while let Some(dir) = cur {
        if oms_safe_fs::has_git_marker(dir) || dir.join(".git").symlink_metadata().is_ok() {
            return true;
        }
        cur = dir.parent();
    }
    false
}

#[derive(Clone, Copy)]
struct ProjectConfigSpec {
    app: PanelApp,
    app_title: &'static str,
    dir_name: &'static str,
    dir_label: &'static str,
    file_name: &'static str,
    rel_path: &'static str,
}

const CODEX_CONFIG_SPEC: ProjectConfigSpec = ProjectConfigSpec {
    app: PanelApp::Codex,
    app_title: "Codex",
    dir_name: ".codex",
    dir_label: ".codex directory",
    file_name: "config.toml",
    rel_path: ".codex/config.toml",
};

const ANTIGRAVITY_CONFIG_SPEC: ProjectConfigSpec = ProjectConfigSpec {
    app: PanelApp::Antigravity,
    app_title: "Antigravity",
    dir_name: ".agents",
    dir_label: ".agents directory",
    file_name: "mcp_config.json",
    rel_path: ".agents/mcp_config.json",
};

fn project_config_spec(app: PanelApp) -> Option<ProjectConfigSpec> {
    match app {
        PanelApp::Claude => None,
        PanelApp::Codex => Some(CODEX_CONFIG_SPEC),
        PanelApp::Antigravity => Some(ANTIGRAVITY_CONFIG_SPEC),
    }
}

fn inspect_git_project_config(
    physical: &Path,
    rel_path: &str,
    stop: &AtomicBool,
) -> Result<GitConfigState> {
    if !has_any_ancestor_git_marker(physical) {
        return Ok(GitConfigState::NotGit);
    }
    let git_bin = find_on_path(Path::new("git")).ok_or_else(|| {
        Error::new(format!(
            "git executable was not found on PATH; cannot verify whether {rel_path} is tracked"
        ))
    })?;
    let physical_str = physical
        .to_str()
        .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
    let run_git = |args: &[&str]| -> Result<CmdOutput> {
        run_bounded_process(
            "git",
            &git_bin,
            args,
            physical,
            GIT_ENV_OVERRIDES,
            VENDOR_TIMEOUT,
            stop,
        )
        .map_err(VendorExecError::into_error)
    };

    let wt_out = run_git(&["-C", physical_str, "rev-parse", "--is-inside-work-tree"])?;
    if wt_out.code != Some(0) || wt_out.stdout.trim() != "true" {
        return Err(Error::new(format!(
            "cannot inspect git worktree at {}",
            physical.display()
        )));
    }

    let ls_out = run_git(&[
        "-C",
        physical_str,
        "--literal-pathspecs",
        "ls-files",
        "--error-unmatch",
        "--",
        rel_path,
    ])?;
    match ls_out.code {
        Some(0) => return Ok(GitConfigState::Tracked),
        Some(1) => {}
        _ => {
            return Err(vendor_failure("git", "ls-files", &ls_out));
        }
    }

    let common_out = run_git(&["-C", physical_str, "rev-parse", "--git-common-dir"])?;
    if common_out.code != Some(0) {
        return Err(vendor_failure(
            "git",
            "rev-parse --git-common-dir",
            &common_out,
        ));
    }
    let common_raw = common_out.stdout.trim();
    require(
        !common_raw.is_empty(),
        "git rev-parse --git-common-dir returned empty path",
    )?;
    let common_git_dir = physical.join(common_raw).canonicalize().map_err(|e| {
        Error::new(format!(
            "cannot resolve git common directory for {}: {e}",
            physical.display()
        ))
    })?;

    let prefix_out = run_git(&["-C", physical_str, "rev-parse", "--show-prefix"])?;
    if prefix_out.code != Some(0) {
        return Err(vendor_failure(
            "git",
            "rev-parse --show-prefix",
            &prefix_out,
        ));
    }
    let prefix = prefix_out.stdout.trim_matches(|c| c == '\n' || c == '\r');
    let exclude_line = if prefix.is_empty() {
        format!("/{rel_path}")
    } else {
        format!("/{prefix}{rel_path}")
    };

    let ign_out = run_git(&["-C", physical_str, "check-ignore", "-q", "--", rel_path])?;
    let already_ignored = match ign_out.code {
        Some(0) => true,
        Some(1) => false,
        _ => return Err(vendor_failure("git", "check-ignore", &ign_out)),
    };

    Ok(GitConfigState::Untracked {
        common_git_dir,
        exclude_line,
        already_ignored,
    })
}

fn inspect_git_codex_config(physical: &Path, stop: &AtomicBool) -> Result<GitConfigState> {
    inspect_git_project_config(physical, CODEX_CONFIG_SPEC.rel_path, stop)
}

fn contains_exact_line(text: &str, target: &str) -> bool {
    text.lines()
        .any(|line| line.strip_suffix('\r').unwrap_or(line) == target)
}

fn append_exclude_line_bytes(prior: Option<&[u8]>, line: &str) -> Result<(Vec<u8>, bool, bool)> {
    let prior_text = match prior {
        Some(bytes) => std::str::from_utf8(bytes)
            .map_err(|_| Error::new("git info/exclude file is not valid UTF-8"))?,
        None => "",
    };
    if contains_exact_line(prior_text, line) {
        return Ok((prior_text.as_bytes().to_vec(), false, false));
    }
    let mut out = String::with_capacity(prior_text.len() + line.len() + 2);
    out.push_str(prior_text);
    let separator_added = !out.is_empty() && !out.ends_with('\n');
    if separator_added {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    Ok((out.into_bytes(), true, separator_added))
}

fn remove_exact_line_bytes(
    raw: &[u8],
    target_line: &str,
    remove_preceding_separator: bool,
) -> Result<Option<(Vec<u8>, bool)>> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| Error::new("git info/exclude file is not valid UTF-8"))?;
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    let mut separator_removed = false;
    let mut pos = 0usize;
    while pos < text.len() {
        let end = match text[pos..].find('\n') {
            Some(rel) => pos + rel + 1,
            None => text.len(),
        };
        let segment = &text[pos..end];
        let bare = segment.strip_suffix('\n').unwrap_or(segment);
        let bare = bare.strip_suffix('\r').unwrap_or(bare);
        if !removed && bare == target_line {
            removed = true;
            if remove_preceding_separator && end == text.len() && out.ends_with('\n') {
                out.pop();
                separator_removed = true;
            }
        } else {
            out.push_str(segment);
        }
        pos = end;
    }
    Ok(removed.then(|| (out.into_bytes(), separator_removed)))
}

struct ExcludeBackup {
    common_git_dir: PathBuf,
    created_info_dir: bool,
    prior_exclude: Option<(Vec<u8>, Stat)>,
    published_stat: Option<Stat>,
}

fn restore_exclude_backup(backup: &ExcludeBackup) -> Result<()> {
    let common_fd = open_dir_nofollow(&backup.common_git_dir, "git common directory")?;
    match &backup.prior_exclude {
        Some((bytes, stat)) => {
            let (info_fd, _) = ensure_subdir_nofollow_at(
                &common_fd,
                OsStr::new("info"),
                0o700,
                "git info directory",
            )?;
            write_regular_file_atomic_at(
                &info_fd,
                OsStr::new("exclude"),
                backup.published_stat.as_ref(),
                bytes,
                stat.st_mode & 0o777,
                "git info/exclude",
            )?;
        }
        None => {
            let Some(info_fd) = open_existing_subdir_nofollow_at(
                &common_fd,
                OsStr::new("info"),
                "git info directory",
            )?
            else {
                return Ok(());
            };
            unlink_regular_file_checked_at(
                &info_fd,
                OsStr::new("exclude"),
                backup.published_stat.as_ref(),
                "git info/exclude",
            )?;
            drop(info_fd);
            if backup.created_info_dir {
                let _ =
                    try_rmdir_if_empty_at(&common_fd, OsStr::new("info"), "git info directory")?;
            }
        }
    }
    Ok(())
}

struct ConfigBackup {
    spec: ProjectConfigSpec,
    physical_repo: PathBuf,
    created_subdir: bool,
    prior_config: Option<(Vec<u8>, Stat)>,
    published_stat: Option<Stat>,
}

fn restore_config_backup(backup: &ConfigBackup) -> Result<()> {
    let spec = backup.spec;
    let repo_fd = open_dir_nofollow(&backup.physical_repo, "project checkout")?;
    match &backup.prior_config {
        Some((bytes, stat)) => {
            let (sub_fd, _) = ensure_subdir_nofollow_at(
                &repo_fd,
                OsStr::new(spec.dir_name),
                0o700,
                spec.dir_label,
            )?;
            write_regular_file_atomic_at(
                &sub_fd,
                OsStr::new(spec.file_name),
                backup.published_stat.as_ref(),
                bytes,
                stat.st_mode & 0o777,
                spec.rel_path,
            )?;
        }
        None => {
            let Some(sub_fd) = open_existing_subdir_nofollow_at(
                &repo_fd,
                OsStr::new(spec.dir_name),
                spec.dir_label,
            )?
            else {
                return Ok(());
            };
            unlink_regular_file_checked_at(
                &sub_fd,
                OsStr::new(spec.file_name),
                backup.published_stat.as_ref(),
                spec.rel_path,
            )?;
            drop(sub_fd);
            if backup.created_subdir {
                let _ = try_rmdir_if_empty_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label)?;
            }
        }
    }
    Ok(())
}

fn rollback_project_panel_files(
    spec: ProjectConfigSpec,
    want_json: bool,
    alias: &str,
    physical: &Path,
    action: &str,
    (config_backup, exclude_backup): (Option<&ConfigBackup>, Option<&ExcludeBackup>),
    primary: Error,
) -> Result<Output> {
    let mut rb_errors = Vec::new();
    if let Some(ex) = exclude_backup
        && let Err(e) = restore_exclude_backup(ex)
    {
        rb_errors.push(e.message);
    }
    if let Some(cfg) = config_backup
        && let Err(e) = restore_config_backup(cfg)
    {
        rb_errors.push(e.message);
    }

    if rb_errors.is_empty() {
        return Err(Error {
            code: primary.code,
            message: format!(
                "{}; rolled back {} panel in {}",
                primary.message, spec.app_title, spec.rel_path
            ),
            publication_uncertain: primary.publication_uncertain,
        });
    }

    let code = if primary.code != 0 { primary.code } else { 1 };
    let message = format!(
        "{}; {} panel rollback for {alias} failed ({}): {} state is uncertain",
        primary.message,
        spec.app_title,
        rb_errors.join("; "),
        spec.rel_path
    );
    let err = Error {
        code,
        message: message.clone(),
        publication_uncertain: true,
    };
    if want_json {
        let (_, _, stderr) = crate::failure(err);
        let mut report =
            panel_action_json(spec.app, alias, physical, PANEL_ENTRY_NAME, action, false);
        report["uncertain"] = json!(true);
        report["error"] = json!(message);
        return Ok((code, json(&report)?, stderr));
    }
    Err(err)
}

fn find_conflicting_project_panel_alias<'a>(
    panels: &'a PanelMap,
    app: PanelApp,
    alias: &str,
    physical: &Path,
) -> Option<&'a str> {
    for (other_alias, rows) in panels {
        if other_alias == alias {
            continue;
        }
        if rows
            .iter()
            .any(|r| r.app == app.as_str() && r.path == physical)
        {
            return Some(other_alias.as_str());
        }
    }
    None
}

fn find_other_panel_sharing_exclude(
    panels: &PanelMap,
    spec: ProjectConfigSpec,
    excluding_alias: &str,
    common_git_dir: &Path,
    exclude_line: &str,
    stop: &AtomicBool,
) -> Option<String> {
    for (other_alias, rows) in panels {
        if other_alias == excluding_alias {
            continue;
        }
        for row in rows {
            if row.app != spec.app.as_str() || !row.path.is_dir() {
                continue;
            }
            if let Ok(GitConfigState::Untracked {
                common_git_dir: other_common,
                exclude_line: other_line,
                ..
            }) = inspect_git_project_config(&row.path, spec.rel_path, stop)
                && other_common == common_git_dir
                && other_line == exclude_line
            {
                return Some(other_alias.clone());
            }
        }
    }
    None
}

fn find_other_panel_owning_exclude_in_common_dir(
    panels: &PanelMap,
    excluding_alias: &str,
    excluding_app: PanelApp,
    common_git_dir: &Path,
    stop: &AtomicBool,
) -> Option<(String, PanelApp)> {
    for (other_alias, rows) in panels {
        for row in rows {
            let Ok(row_app) = PanelApp::parse(&row.app) else {
                continue;
            };
            if other_alias == excluding_alias && row_app == excluding_app {
                continue;
            }
            let Some(other_spec) = project_config_spec(row_app) else {
                continue;
            };
            if row.created_exclude != Some(true) || !row.path.is_dir() {
                continue;
            }
            if let Ok(GitConfigState::Untracked {
                common_git_dir: other_common,
                ..
            }) = inspect_git_project_config(&row.path, other_spec.rel_path, stop)
                && other_common == common_git_dir
            {
                return Some((other_alias.clone(), row_app));
            }
        }
    }
    None
}

fn apply_install_config_write(
    spec: ProjectConfigSpec,
    physical: &Path,
    prior_config: Option<(Vec<u8>, Stat)>,
    candidate_bytes: &[u8],
) -> Result<(bool, bool, ConfigBackup)> {
    let repo_fd = open_dir_nofollow(physical, "project checkout")?;
    let (sub_fd, created_dir) =
        ensure_subdir_nofollow_at(&repo_fd, OsStr::new(spec.dir_name), 0o700, spec.dir_label)?;
    let before_stat = prior_config.as_ref().map(|(_, st)| st);
    let target_mode = match before_stat {
        Some(st) => st.st_mode & 0o777,
        None => 0o600,
    };
    let created_file = before_stat.is_none();
    let published_cfg_stat = match write_regular_file_atomic_at(
        &sub_fd,
        OsStr::new(spec.file_name),
        before_stat,
        candidate_bytes,
        target_mode,
        spec.rel_path,
    ) {
        Ok(st) => st,
        Err(write_err) => {
            drop(sub_fd);
            if created_dir {
                let _ = try_rmdir_if_empty_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label);
            }
            return Err(write_err);
        }
    };
    let backup = ConfigBackup {
        spec,
        physical_repo: physical.to_path_buf(),
        created_subdir: created_dir,
        prior_config,
        published_stat: Some(published_cfg_stat),
    };
    Ok((created_file, created_dir, backup))
}

fn apply_install_exclude_step(
    common_git_dir: PathBuf,
    exclude_line: &str,
) -> Result<(bool, bool, Option<ExcludeBackup>)> {
    let common_fd = open_dir_nofollow(&common_git_dir, "git common directory")?;
    let (info_fd, created_info_dir) =
        ensure_subdir_nofollow_at(&common_fd, OsStr::new("info"), 0o700, "git info directory")?;
    let prior_exclude =
        read_regular_file_nofollow_at(&info_fd, OsStr::new("exclude"), "git info/exclude")?;
    let (next_bytes, added, sep_added) = append_exclude_line_bytes(
        prior_exclude.as_ref().map(|(b, _)| b.as_slice()),
        exclude_line,
    )?;
    if added {
        let before_stat = prior_exclude.as_ref().map(|(_, st)| st);
        let mode = match before_stat {
            Some(st) => st.st_mode & 0o777,
            None => 0o600,
        };
        let published_excl_stat = write_regular_file_atomic_at(
            &info_fd,
            OsStr::new("exclude"),
            before_stat,
            &next_bytes,
            mode,
            "git info/exclude",
        )?;
        Ok((
            true,
            sep_added,
            Some(ExcludeBackup {
                common_git_dir,
                created_info_dir,
                prior_exclude,
                published_stat: Some(published_excl_stat),
            }),
        ))
    } else {
        if created_info_dir {
            drop(info_fd);
            let _ = try_rmdir_if_empty_at(&common_fd, OsStr::new("info"), "git info directory")?;
        }
        Ok((false, false, None))
    }
}

fn apply_uninstall_config_clean_remove(
    spec: ProjectConfigSpec,
    target_repo: &Path,
    remaining_bytes: Option<Vec<u8>>,
    prior_config: Option<(Vec<u8>, Stat)>,
) -> Result<(bool, Option<ConfigBackup>)> {
    let repo_fd = open_dir_nofollow(target_repo, "project checkout")?;
    let Some(sub_fd) =
        open_existing_subdir_nofollow_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label)?
    else {
        return Ok((false, None));
    };
    let before_stat = prior_config.as_ref().map(|(_, st)| st);
    let published_cfg_stat = match remaining_bytes {
        Some(bytes) => {
            let mode = before_stat.map_or(0o600, |st| st.st_mode & 0o777);
            Some(write_regular_file_atomic_at(
                &sub_fd,
                OsStr::new(spec.file_name),
                before_stat,
                &bytes,
                mode,
                spec.rel_path,
            )?)
        }
        None => {
            unlink_regular_file_checked_at(
                &sub_fd,
                OsStr::new(spec.file_name),
                before_stat,
                spec.rel_path,
            )?;
            None
        }
    };
    Ok((
        true,
        Some(ConfigBackup {
            spec,
            physical_repo: target_repo.to_path_buf(),
            created_subdir: false,
            prior_config,
            published_stat: published_cfg_stat,
        }),
    ))
}

fn apply_uninstall_exclude_step(
    panels: &mut PanelMap,
    spec: ProjectConfigSpec,
    alias: &str,
    git_state: GitConfigState,
    created_exclude: bool,
    exclude_separator_added: bool,
    stop: &AtomicBool,
) -> Result<Option<ExcludeBackup>> {
    if !created_exclude {
        return Ok(None);
    }
    let GitConfigState::Untracked {
        common_git_dir,
        exclude_line,
        ..
    } = git_state
    else {
        return Ok(None);
    };

    if let Some(handover_alias) =
        find_other_panel_sharing_exclude(panels, spec, alias, &common_git_dir, &exclude_line, stop)
    {
        if let Some(other_rows) = panels.get_mut(&handover_alias)
            && let Some(other_row) = other_rows.iter_mut().find(|r| r.app == spec.app.as_str())
        {
            other_row.created_exclude = Some(true);
            if exclude_separator_added {
                other_row.exclude_separator_added = Some(true);
            }
        }
        return Ok(None);
    }

    let mut separator_removed = false;
    let mut exclude_backup = None;
    let common_fd = open_dir_nofollow(&common_git_dir, "git common directory")?;
    if let Some(info_fd) =
        open_existing_subdir_nofollow_at(&common_fd, OsStr::new("info"), "git info directory")?
        && let Some((prior_bytes, prior_stat)) =
            read_regular_file_nofollow_at(&info_fd, OsStr::new("exclude"), "git info/exclude")?
        && let Some((next_bytes, sep_removed)) =
            remove_exact_line_bytes(&prior_bytes, &exclude_line, exclude_separator_added)?
    {
        separator_removed = sep_removed;
        let published_excl_stat = if next_bytes.is_empty() {
            unlink_regular_file_checked_at(
                &info_fd,
                OsStr::new("exclude"),
                Some(&prior_stat),
                "git info/exclude",
            )?;
            None
        } else {
            Some(write_regular_file_atomic_at(
                &info_fd,
                OsStr::new("exclude"),
                Some(&prior_stat),
                &next_bytes,
                prior_stat.st_mode & 0o777,
                "git info/exclude",
            )?)
        };
        exclude_backup = Some(ExcludeBackup {
            common_git_dir: common_git_dir.clone(),
            created_info_dir: false,
            prior_exclude: Some((prior_bytes, prior_stat)),
            published_stat: published_excl_stat,
        });
    }

    if exclude_separator_added
        && !separator_removed
        && let Some((sep_alias, sep_app)) = find_other_panel_owning_exclude_in_common_dir(
            panels,
            alias,
            spec.app,
            &common_git_dir,
            stop,
        )
        && let Some(other_rows) = panels.get_mut(&sep_alias)
        && let Some(other_row) = other_rows.iter_mut().find(|r| r.app == sep_app.as_str())
    {
        other_row.exclude_separator_added = Some(true);
    }

    Ok(exclude_backup)
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
    let out = run_claude_command(claude_bin, &["mcp", "get", PANEL_ENTRY_NAME], workdir, stop)
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
    physical: &Path,
    meta: RowMeta,
) {
    let record = match meta {
        RowMeta::Claude => PanelRecord {
            app: app.as_str().to_owned(),
            scope: app.expected_scope().to_owned(),
            name: PANEL_ENTRY_NAME.to_owned(),
            path: physical.to_path_buf(),
            block_sha256: None,
            entry_sha256: None,
            created_config: None,
            created_codex_dir: None,
            created_agents_dir: None,
            created_exclude: None,
            separator_added: None,
            exclude_separator_added: None,
            created_mcp_servers: None,
        },
        RowMeta::Codex(m) => PanelRecord {
            app: app.as_str().to_owned(),
            scope: app.expected_scope().to_owned(),
            name: PANEL_ENTRY_NAME.to_owned(),
            path: physical.to_path_buf(),
            block_sha256: Some(m.block_sha256),
            entry_sha256: None,
            created_config: Some(m.created_config),
            created_codex_dir: Some(m.created_codex_dir),
            created_agents_dir: None,
            created_exclude: Some(m.created_exclude),
            separator_added: m.separator_added.then_some(true),
            exclude_separator_added: m.exclude_separator_added.then_some(true),
            created_mcp_servers: None,
        },
        RowMeta::Antigravity(m) => PanelRecord {
            app: app.as_str().to_owned(),
            scope: app.expected_scope().to_owned(),
            name: PANEL_ENTRY_NAME.to_owned(),
            path: physical.to_path_buf(),
            block_sha256: None,
            entry_sha256: Some(m.entry_sha256),
            created_config: Some(m.created_config),
            created_codex_dir: None,
            created_agents_dir: Some(m.created_agents_dir),
            created_exclude: Some(m.created_exclude),
            separator_added: None,
            exclude_separator_added: m.exclude_separator_added.then_some(true),
            created_mcp_servers: m.created_mcp_servers.then_some(true),
        },
    };
    let rows = panels.entry(alias.to_owned()).or_default();
    if let Some(slot) = rows.iter_mut().find(|r| r.app == app.as_str()) {
        *slot = record;
    } else {
        rows.push(record);
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
    let rollback = run_claude_command(
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

enum CodexInstallPlan {
    NoOp,
    Adopt {
        block_sha256: String,
    },
    Write {
        candidate_bytes: Vec<u8>,
        block_sha256: String,
        replaced_existing_block: bool,
        added_separator: bool,
    },
}

struct PreparedCodexInstall {
    plan: CodexInstallPlan,
    codex_dir_existed: bool,
    prior_config: Option<(Vec<u8>, Stat)>,
    repo_root: Option<PathBuf>,
    exclude_target: Option<(PathBuf, String)>,
}

fn prepare_codex_install(
    alias: &str,
    physical: &Path,
    launcher: &Path,
    recorded_row: Option<&PanelRecord>,
    stop: &AtomicBool,
) -> Result<PreparedCodexInstall> {
    let repo_fd = open_dir_nofollow(physical, "project checkout")?;
    let codex_fd =
        open_existing_subdir_nofollow_at(&repo_fd, OsStr::new(".codex"), ".codex directory")?;
    let codex_dir_existed = codex_fd.is_some();
    let prior_config = match &codex_fd {
        Some(fd) => {
            read_regular_file_nofollow_at(fd, OsStr::new("config.toml"), ".codex/config.toml")?
        }
        None => None,
    };

    let (repo_root, exclude_target) = match inspect_git_codex_config(physical, stop)? {
        GitConfigState::NotGit => (None, None),
        GitConfigState::Tracked => {
            return Err(Error::new(format!(
                "refusing to modify tracked file .codex/config.toml in {}; untrack it first",
                physical.display()
            )));
        }
        GitConfigState::Untracked {
            common_git_dir,
            exclude_line,
            already_ignored,
        } => {
            let root = common_git_dir.parent().map(Path::to_path_buf);
            let target = (!already_ignored).then_some((common_git_dir, exclude_line));
            (root, target)
        }
    };

    let resource_version = codex_panel_resource_version();
    let expected_block = codex_panel_block(launcher, physical)?;
    let expected_sha = oms_panel_core::digest(expected_block.as_bytes());

    let (plan, outside_table) = if let Some((ref prior_bytes, _)) = prior_config {
        let prior_str = std::str::from_utf8(prior_bytes)
            .map_err(|_| Error::new("Invalid .codex/config.toml: file is not valid UTF-8"))?;
        let parsed_prior = oms_toml::parse_bounded(prior_bytes, MAX_CONFIG_BYTES)
            .map_err(|e| Error::new(format!("Invalid .codex/config.toml: {e}")))?;
        let split = split_codex_panel_block(prior_str)?;
        let outside = format!("{}{}", split.before, split.after);
        let outside_table =
            oms_toml::parse_bounded(outside.as_bytes(), MAX_CONFIG_BYTES).map_err(|e| {
                Error::new(format!("Invalid .codex/config.toml outside OMS block: {e}"))
            })?;

        match outside_table.get("mcp_servers") {
            None => {}
            Some(oms_toml::Value::Table(servers)) => {
                if servers.contains_key(PANEL_ENTRY_NAME) {
                    return Err(Error::new(format!(
                        "an unmanaged mcp_servers.{PANEL_ENTRY_NAME} entry exists in .codex/config.toml; remove it first"
                    )));
                }
            }
            Some(_) => {
                return Err(Error::new(
                    "Invalid .codex/config.toml: mcp_servers must be a TOML table",
                ));
            }
        }

        let keep_empty_mcp = outside_table.contains_key("mcp_servers");
        let stripped_prior = table_without_oms_panel(parsed_prior.clone(), keep_empty_mcp)?;
        if split.block.is_none() && parsed_prior != outside_table {
            return Err(Error::new(format!(
                "an unmanaged mcp_servers.{PANEL_ENTRY_NAME} entry exists in .codex/config.toml; remove it first"
            )));
        }
        require(
            stripped_prior == outside_table,
            "OMS block in .codex/config.toml interacts with surrounding TOML statements; refusing to modify",
        )?;

        if let Some(on_disk_block) = split.block {
            let on_disk_sha = oms_panel_core::digest(on_disk_block.as_bytes());
            if let Some(existing) = recorded_row {
                if existing.block_sha256.as_deref() != Some(on_disk_sha.as_str()) {
                    return Err(Error::new(format!(
                        "OMS block in .codex/config.toml for '{alias}' was modified outside OMS; restore it or run `oms panel --uninstall-panel codex --project {alias}` first"
                    )));
                }
                if on_disk_block == expected_block && exclude_target.is_none() {
                    let verified = oms_toml::parse_bounded(prior_bytes, MAX_CONFIG_BYTES)
                        .map_err(|e| Error::new(format!("Invalid .codex/config.toml: {e}")))?;
                    let expected_full = table_with_expected_oms_panel(
                        outside_table.clone(),
                        launcher,
                        physical,
                        resource_version,
                    )?;
                    require(
                        verified == expected_full,
                        "existing .codex/config.toml did not parse to the expected mcp_servers.oms-panel table",
                    )?;
                    (CodexInstallPlan::NoOp, outside_table)
                } else {
                    let candidate = format!("{}{expected_block}{}", split.before, split.after);
                    (
                        CodexInstallPlan::Write {
                            candidate_bytes: candidate.into_bytes(),
                            block_sha256: expected_sha,
                            replaced_existing_block: true,
                            added_separator: false,
                        },
                        outside_table,
                    )
                }
            } else if on_disk_block == expected_block {
                let verified = oms_toml::parse_bounded(prior_bytes, MAX_CONFIG_BYTES)
                    .map_err(|e| Error::new(format!("Invalid .codex/config.toml: {e}")))?;
                let expected_full = table_with_expected_oms_panel(
                    outside_table.clone(),
                    launcher,
                    physical,
                    resource_version,
                )?;
                require(
                    verified == expected_full,
                    "existing .codex/config.toml did not parse to the expected mcp_servers.oms-panel table",
                )?;
                (
                    CodexInstallPlan::Adopt {
                        block_sha256: expected_sha,
                    },
                    outside_table,
                )
            } else {
                return Err(Error::new(format!(
                    "an unrecorded or edited OMS block in .codex/config.toml exists for '{alias}'; remove or restore it first"
                )));
            }
        } else {
            let needs_separator = !prior_str.is_empty() && !prior_str.ends_with('\n');
            let candidate = if needs_separator {
                format!("{prior_str}\n{expected_block}")
            } else {
                format!("{prior_str}{expected_block}")
            };
            (
                CodexInstallPlan::Write {
                    candidate_bytes: candidate.into_bytes(),
                    block_sha256: expected_sha,
                    replaced_existing_block: false,
                    added_separator: needs_separator,
                },
                outside_table,
            )
        }
    } else {
        (
            CodexInstallPlan::Write {
                candidate_bytes: expected_block.into_bytes(),
                block_sha256: expected_sha,
                replaced_existing_block: false,
                added_separator: false,
            },
            oms_toml::Table::new(),
        )
    };

    if let CodexInstallPlan::Write {
        ref candidate_bytes,
        ..
    } = plan
    {
        let parsed_candidate =
            oms_toml::parse_bounded(candidate_bytes, MAX_CONFIG_BYTES).map_err(|e| {
                Error::new(format!(
                    "generated .codex/config.toml failed TOML parse: {e}"
                ))
            })?;
        let expected_full =
            table_with_expected_oms_panel(outside_table, launcher, physical, resource_version)?;
        require(
            parsed_candidate == expected_full,
            "generated .codex/config.toml did not parse to the expected mcp_servers.oms-panel table",
        )?;
    }

    Ok(PreparedCodexInstall {
        plan,
        codex_dir_existed,
        prior_config,
        repo_root,
        exclude_target,
    })
}

fn format_codex_install_text(
    alias: &str,
    physical: &Path,
    verb_prefix: &str,
    trust: &CodexTrustStatus,
) -> Vec<u8> {
    match trust {
        CodexTrustStatus::Trusted => format!(
            "{verb_prefix} codex panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
            physical.display()
        )
        .into_bytes(),
        CodexTrustStatus::Untrusted { trust_target } => format!(
            "{verb_prefix} codex panel {PANEL_ENTRY_NAME} for {alias}: {} (inactive: {CODEX_UNTRUSTED_REASON})\nhint: {}\n",
            physical.display(),
            codex_untrusted_hint(trust_target)
        )
        .into_bytes(),
        CodexTrustStatus::Unknown { hint } => format!(
            "{verb_prefix} codex panel {PANEL_ENTRY_NAME} for {alias}: {} (inactive: {CODEX_TRUST_UNKNOWN_REASON})\nhint: {hint}\n",
            physical.display(),
        )
        .into_bytes(),
    }
}

enum AntigravityInstallPlan {
    NoOp,
    Adopt {
        entry_sha256: String,
    },
    EnsureExcludeOnly {
        entry_sha256: String,
    },
    Write {
        candidate_bytes: Vec<u8>,
        entry_sha256: String,
        created_mcp_servers_now: bool,
    },
}

struct PreparedAntigravityInstall {
    plan: AntigravityInstallPlan,
    agents_dir_existed: bool,
    prior_config: Option<(Vec<u8>, Stat)>,
    exclude_target: Option<(PathBuf, String)>,
}

fn prepare_antigravity_install(
    alias: &str,
    physical: &Path,
    launcher: &Path,
    recorded_row: Option<&PanelRecord>,
    stop: &AtomicBool,
) -> Result<PreparedAntigravityInstall> {
    let spec = ANTIGRAVITY_CONFIG_SPEC;
    let repo_fd = open_dir_nofollow(physical, "project checkout")?;
    let agents_fd =
        open_existing_subdir_nofollow_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label)?;
    let agents_dir_existed = agents_fd.is_some();
    let prior_config = match &agents_fd {
        Some(fd) => read_regular_file_nofollow_at(fd, OsStr::new(spec.file_name), spec.rel_path)?,
        None => None,
    };

    let exclude_target = match inspect_git_project_config(physical, spec.rel_path, stop)? {
        GitConfigState::NotGit => None,
        GitConfigState::Tracked => {
            return Err(Error::new(format!(
                "refusing to modify tracked file {} in {}; untrack it first",
                spec.rel_path,
                physical.display()
            )));
        }
        GitConfigState::Untracked {
            common_git_dir,
            exclude_line,
            already_ignored,
        } => (!already_ignored).then_some((common_git_dir, exclude_line)),
    };

    let expected_entry = expected_antigravity_panel_entry(launcher, physical)?;
    let expected_sha = canonical_json_sha256(&expected_entry)?;

    let plan = if let Some((ref prior_bytes, _)) = prior_config {
        let mut updated_root = parse_strict_json_object_bounded(prior_bytes, spec.rel_path)?;
        let (had_mcp_servers, on_disk_entry) = match updated_root.get("mcpServers") {
            None => (false, None),
            Some(serde_json::Value::Object(servers)) => {
                (true, servers.get(PANEL_ENTRY_NAME).cloned())
            }
            Some(_) => {
                return Err(Error::new(
                    "Invalid .agents/mcp_config.json: mcpServers must be a JSON object",
                ));
            }
        };

        if let Some(entry_val) = on_disk_entry {
            if !entry_val.is_object() {
                return Err(Error::new(if recorded_row.is_some() {
                    format!(
                        "mcpServers.{PANEL_ENTRY_NAME} in .agents/mcp_config.json for '{alias}' was modified outside OMS; restore it or run `oms panel --uninstall-panel antigravity --project {alias}` first"
                    )
                } else {
                    format!(
                        "an unmanaged mcpServers.{PANEL_ENTRY_NAME} entry exists in .agents/mcp_config.json; remove it first"
                    )
                }));
            }
            let on_disk_sha = canonical_json_sha256(&entry_val)?;
            if let Some(existing) = recorded_row {
                if existing.entry_sha256.as_deref() != Some(on_disk_sha.as_str()) {
                    return Err(Error::new(format!(
                        "mcpServers.{PANEL_ENTRY_NAME} in .agents/mcp_config.json for '{alias}' was modified outside OMS; restore it or run `oms panel --uninstall-panel antigravity --project {alias}` first"
                    )));
                }
                if on_disk_sha == expected_sha {
                    if exclude_target.is_none() {
                        AntigravityInstallPlan::NoOp
                    } else {
                        AntigravityInstallPlan::EnsureExcludeOnly {
                            entry_sha256: expected_sha,
                        }
                    }
                } else {
                    let servers = updated_root
                        .get_mut("mcpServers")
                        .and_then(serde_json::Value::as_object_mut)
                        .ok_or_else(|| {
                            Error::new(
                                "Invalid .agents/mcp_config.json: mcpServers must be a JSON object",
                            )
                        })?;
                    servers.insert(PANEL_ENTRY_NAME.into(), expected_entry);
                    let candidate_bytes = encode_pretty_2space_json(&updated_root)?;
                    let verified = parse_strict_json_object_bounded(
                        &candidate_bytes,
                        "generated .agents/mcp_config.json",
                    )?;
                    require(
                        verified == updated_root,
                        "generated .agents/mcp_config.json did not parse to the expected mcpServers.oms-panel object",
                    )?;
                    AntigravityInstallPlan::Write {
                        candidate_bytes,
                        entry_sha256: expected_sha,
                        created_mcp_servers_now: false,
                    }
                }
            } else if on_disk_sha == expected_sha {
                AntigravityInstallPlan::Adopt {
                    entry_sha256: expected_sha,
                }
            } else {
                return Err(Error::new(format!(
                    "an unmanaged mcpServers.{PANEL_ENTRY_NAME} entry exists in .agents/mcp_config.json; remove it first"
                )));
            }
        } else {
            let entry = updated_root
                .entry("mcpServers")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            let serde_json::Value::Object(servers) = entry else {
                return Err(Error::new(
                    "Invalid .agents/mcp_config.json: mcpServers must be a JSON object",
                ));
            };
            servers.insert(PANEL_ENTRY_NAME.into(), expected_entry);
            let candidate_bytes = encode_pretty_2space_json(&updated_root)?;
            let verified = parse_strict_json_object_bounded(
                &candidate_bytes,
                "generated .agents/mcp_config.json",
            )?;
            require(
                verified == updated_root,
                "generated .agents/mcp_config.json did not parse to the expected mcpServers.oms-panel object",
            )?;
            AntigravityInstallPlan::Write {
                candidate_bytes,
                entry_sha256: expected_sha,
                created_mcp_servers_now: !had_mcp_servers,
            }
        }
    } else {
        let mut servers = serde_json::Map::new();
        servers.insert(PANEL_ENTRY_NAME.into(), expected_entry);
        let mut updated_root = serde_json::Map::new();
        updated_root.insert("mcpServers".into(), serde_json::Value::Object(servers));
        let candidate_bytes = encode_pretty_2space_json(&updated_root)?;
        let verified = parse_strict_json_object_bounded(
            &candidate_bytes,
            "generated .agents/mcp_config.json",
        )?;
        require(
            verified == updated_root,
            "generated .agents/mcp_config.json did not parse to the expected mcpServers.oms-panel object",
        )?;
        AntigravityInstallPlan::Write {
            candidate_bytes,
            entry_sha256: expected_sha,
            created_mcp_servers_now: false,
        }
    };

    Ok(PreparedAntigravityInstall {
        plan,
        agents_dir_existed,
        prior_config,
        exclude_target,
    })
}

pub(super) fn install_panel(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let app = PanelApp::parse(&options.required("install-panel")?)?;
    let alias = options.required("project")?;
    check_alias(&alias)?;
    let dry_run = options.has("dry-run");
    let want_json = options.has("json");

    let (target, explicit) = resolve_writer_path()?;
    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
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
        let panels = read_panels_at(&dir_fd)?.map(|(p, _)| p).unwrap_or_default();
        let configured = map.get(&alias).ok_or_else(|| unknown_alias_error(&alias))?;
        let configured_str = configured
            .to_str()
            .ok_or_else(|| Error::new("project path must be valid UTF-8"))?;
        let physical = validate_checkout(Some(configured_str), cwd)?;
        let recorded_row = panels
            .get(&alias)
            .and_then(|rows| rows.iter().find(|r| r.app == app.as_str()));
        if let Some(existing) = recorded_row
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
                let claude_bin = resolve_claude_program(cwd, true)?;
                let entry_json = claude_panel_entry_json(&launcher, &physical)?;
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
                        "install",
                        false,
                    );
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
                if let Some(existing_alias) =
                    find_conflicting_project_panel_alias(&panels, app, &alias, &physical)
                {
                    return Err(Error::new(format!(
                        "checkout {} already has a recorded codex panel under project alias '{existing_alias}'; use '{existing_alias}' or run `oms panel --uninstall-panel codex --project {existing_alias}` first",
                        physical.display()
                    )));
                }
                let prepared =
                    prepare_codex_install(&alias, &physical, &launcher, recorded_row, stop)?;
                let trust =
                    check_codex_project_trust(cwd, &physical, prepared.repo_root.as_deref());
                if want_json {
                    let mut report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
                        "install",
                        false,
                    );
                    attach_codex_trust_fields(&mut report, &trust);
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format_codex_install_text(&alias, &physical, "would install", &trust),
                    vec![],
                ));
            }
            PanelApp::Antigravity => {
                if let Some(existing_alias) =
                    find_conflicting_project_panel_alias(&panels, app, &alias, &physical)
                {
                    return Err(Error::new(format!(
                        "checkout {} already has a recorded antigravity panel under project alias '{existing_alias}'; use '{existing_alias}' or run `oms panel --uninstall-panel antigravity --project {existing_alias}` first",
                        physical.display()
                    )));
                }
                let _prepared =
                    prepare_antigravity_install(&alias, &physical, &launcher, recorded_row, stop)?;
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
                        "install",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!(
                        "would install {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                        app.as_str(),
                        physical.display()
                    )
                    .into_bytes(),
                    vec![],
                ));
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
            let claude_bin = resolve_claude_program(cwd, false)?;
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
                            PANEL_ENTRY_NAME,
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
                upsert_panel_row(&mut panels, &alias, app, &physical, RowMeta::Claude);
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
                        "adopted",
                        true,
                    );
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

            let add_res = run_claude_command(
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

            upsert_panel_row(&mut panels, &alias, app, &physical, RowMeta::Claude);
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
                    panel_action_json(app, &alias, &physical, PANEL_ENTRY_NAME, "install", true);
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
            let spec = CODEX_CONFIG_SPEC;
            if let Some(existing_alias) =
                find_conflicting_project_panel_alias(&panels, app, &alias, &physical)
            {
                return Err(Error::new(format!(
                    "checkout {} already has a recorded codex panel under project alias '{existing_alias}'; use '{existing_alias}' or run `oms panel --uninstall-panel codex --project {existing_alias}` first",
                    physical.display()
                )));
            }
            let prepared =
                prepare_codex_install(&alias, &physical, &launcher, recorded_row.as_ref(), stop)?;
            let trust = check_codex_project_trust(cwd, &physical, prepared.repo_root.as_deref());

            if matches!(prepared.plan, CodexInstallPlan::NoOp) {
                if want_json {
                    let mut report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
                        "install",
                        false,
                    );
                    attach_codex_trust_fields(&mut report, &trust);
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format_codex_install_text(&alias, &physical, "already installed", &trust),
                    vec![],
                ));
            }

            if stop.load(Ordering::Acquire) {
                return Err(Error {
                    code: 130,
                    message: "interrupted before writing .codex/config.toml".into(),
                    publication_uncertain: false,
                });
            }

            let mut config_backup: Option<ConfigBackup> = None;
            let mut exclude_backup: Option<ExcludeBackup> = None;

            let (
                action_str,
                text_prefix,
                block_sha256,
                created_config_now,
                created_codex_dir_now,
                replaced_existing_block,
                added_separator_now,
            ) = match prepared.plan {
                CodexInstallPlan::NoOp => unreachable!(),
                CodexInstallPlan::Adopt { block_sha256 } => (
                    "adopted",
                    "adopted existing",
                    block_sha256,
                    false,
                    false,
                    false,
                    false,
                ),
                CodexInstallPlan::Write {
                    candidate_bytes,
                    block_sha256,
                    replaced_existing_block,
                    added_separator,
                } => {
                    let (created_file, created_dir, backup) = apply_install_config_write(
                        spec,
                        &physical,
                        prepared.prior_config,
                        &candidate_bytes,
                    )?;
                    config_backup = Some(backup);
                    (
                        "install",
                        "installed",
                        block_sha256,
                        created_file,
                        created_dir,
                        replaced_existing_block,
                        added_separator,
                    )
                }
            };

            let mut added_exclude_now = false;
            let mut added_exclude_sep_now = false;
            if let Some((common_git_dir, exclude_line)) = prepared.exclude_target {
                match apply_install_exclude_step(common_git_dir, &exclude_line) {
                    Ok((added, sep_added, ex_backup)) => {
                        added_exclude_now = added;
                        added_exclude_sep_now = sep_added;
                        exclude_backup = ex_backup;
                    }
                    Err(ex_err) => {
                        if config_backup.is_some() {
                            return rollback_project_panel_files(
                                spec,
                                want_json,
                                &alias,
                                &physical,
                                action_str,
                                (config_backup.as_ref(), None),
                                ex_err,
                            );
                        }
                        return Err(ex_err);
                    }
                }
            }

            let created_config = match &recorded_row {
                Some(prev) if !created_config_now => prev.created_config == Some(true),
                _ => created_config_now,
            };
            let created_codex_dir = match &recorded_row {
                Some(prev) if prepared.codex_dir_existed => prev.created_codex_dir == Some(true),
                _ => created_codex_dir_now,
            };
            let separator_added = if replaced_existing_block {
                recorded_row
                    .as_ref()
                    .is_some_and(|prev| prev.separator_added == Some(true))
            } else {
                added_separator_now
            };
            let (created_exclude, exclude_separator_added) = if added_exclude_now {
                (true, added_exclude_sep_now)
            } else if let Some(prev) = &recorded_row
                && prev.created_exclude == Some(true)
            {
                (true, prev.exclude_separator_added == Some(true))
            } else {
                (false, false)
            };

            upsert_panel_row(
                &mut panels,
                &alias,
                app,
                &physical,
                RowMeta::Codex(CodexRowMeta {
                    block_sha256,
                    created_config,
                    created_codex_dir,
                    created_exclude,
                    separator_added,
                    exclude_separator_added,
                }),
            );
            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                if config_backup.is_some() || exclude_backup.is_some() {
                    return rollback_project_panel_files(
                        spec,
                        want_json,
                        &alias,
                        &physical,
                        action_str,
                        (config_backup.as_ref(), exclude_backup.as_ref()),
                        record_err,
                    );
                }
                return Err(record_err);
            }

            if want_json {
                let mut report =
                    panel_action_json(app, &alias, &physical, PANEL_ENTRY_NAME, action_str, true);
                attach_codex_trust_fields(&mut report, &trust);
                return Ok((0, json(&report)?, vec![]));
            }
            Ok((
                0,
                format_codex_install_text(&alias, &physical, text_prefix, &trust),
                vec![],
            ))
        }
        PanelApp::Antigravity => {
            let spec = ANTIGRAVITY_CONFIG_SPEC;
            if let Some(existing_alias) =
                find_conflicting_project_panel_alias(&panels, app, &alias, &physical)
            {
                return Err(Error::new(format!(
                    "checkout {} already has a recorded antigravity panel under project alias '{existing_alias}'; use '{existing_alias}' or run `oms panel --uninstall-panel antigravity --project {existing_alias}` first",
                    physical.display()
                )));
            }
            let prepared = prepare_antigravity_install(
                &alias,
                &physical,
                &launcher,
                recorded_row.as_ref(),
                stop,
            )?;

            if matches!(prepared.plan, AntigravityInstallPlan::NoOp) {
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &physical,
                        PANEL_ENTRY_NAME,
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

            if stop.load(Ordering::Acquire) {
                return Err(Error {
                    code: 130,
                    message: "interrupted before writing .agents/mcp_config.json".into(),
                    publication_uncertain: false,
                });
            }

            let mut config_backup: Option<ConfigBackup> = None;
            let mut exclude_backup: Option<ExcludeBackup> = None;

            enum AgyWriteOutcome {
                Adopted(String),
                ExcludeOnly(String),
                Written {
                    entry_sha256: String,
                    created_config_now: bool,
                    created_agents_dir_now: bool,
                    created_mcp_servers_now: bool,
                },
            }

            let (action_str, text_prefix, outcome) = match prepared.plan {
                AntigravityInstallPlan::NoOp => unreachable!(),
                AntigravityInstallPlan::Adopt { entry_sha256 } => (
                    "adopted",
                    "adopted existing",
                    AgyWriteOutcome::Adopted(entry_sha256),
                ),
                AntigravityInstallPlan::EnsureExcludeOnly { entry_sha256 } => (
                    "install",
                    "installed",
                    AgyWriteOutcome::ExcludeOnly(entry_sha256),
                ),
                AntigravityInstallPlan::Write {
                    candidate_bytes,
                    entry_sha256,
                    created_mcp_servers_now,
                } => {
                    let (created_file, created_dir, backup) = apply_install_config_write(
                        spec,
                        &physical,
                        prepared.prior_config,
                        &candidate_bytes,
                    )?;
                    config_backup = Some(backup);
                    (
                        "install",
                        "installed",
                        AgyWriteOutcome::Written {
                            entry_sha256,
                            created_config_now: created_file,
                            created_agents_dir_now: created_dir,
                            created_mcp_servers_now,
                        },
                    )
                }
            };

            let mut added_exclude_now = false;
            let mut added_exclude_sep_now = false;
            if let Some((common_git_dir, exclude_line)) = prepared.exclude_target {
                match apply_install_exclude_step(common_git_dir, &exclude_line) {
                    Ok((added, sep_added, ex_backup)) => {
                        added_exclude_now = added;
                        added_exclude_sep_now = sep_added;
                        exclude_backup = ex_backup;
                    }
                    Err(ex_err) => {
                        if config_backup.is_some() {
                            return rollback_project_panel_files(
                                spec,
                                want_json,
                                &alias,
                                &physical,
                                action_str,
                                (config_backup.as_ref(), None),
                                ex_err,
                            );
                        }
                        return Err(ex_err);
                    }
                }
            }

            let (created_exclude, exclude_separator_added) = if added_exclude_now {
                (true, added_exclude_sep_now)
            } else if let Some(prev) = &recorded_row
                && prev.created_exclude == Some(true)
            {
                (true, prev.exclude_separator_added == Some(true))
            } else {
                (false, false)
            };

            let agy_meta = match outcome {
                AgyWriteOutcome::Adopted(entry_sha256) => AntigravityRowMeta {
                    entry_sha256,
                    created_config: false,
                    created_agents_dir: false,
                    created_exclude,
                    exclude_separator_added,
                    created_mcp_servers: false,
                },
                AgyWriteOutcome::ExcludeOnly(entry_sha256) => {
                    let prev = recorded_row
                        .as_ref()
                        .expect("recorded row for exclude-only");
                    AntigravityRowMeta {
                        entry_sha256,
                        created_config: prev.created_config == Some(true),
                        created_agents_dir: prev.created_agents_dir == Some(true),
                        created_exclude,
                        exclude_separator_added,
                        created_mcp_servers: prev.created_mcp_servers == Some(true),
                    }
                }
                AgyWriteOutcome::Written {
                    entry_sha256,
                    created_config_now,
                    created_agents_dir_now,
                    created_mcp_servers_now,
                } => {
                    let created_config = match &recorded_row {
                        Some(prev) if !created_config_now => prev.created_config == Some(true),
                        _ => created_config_now,
                    };
                    let created_agents_dir = match &recorded_row {
                        Some(prev) if prepared.agents_dir_existed => {
                            prev.created_agents_dir == Some(true)
                        }
                        _ => created_agents_dir_now,
                    };
                    let created_mcp_servers = !created_config
                        && (created_mcp_servers_now
                            || recorded_row
                                .as_ref()
                                .is_some_and(|prev| prev.created_mcp_servers == Some(true)));
                    AntigravityRowMeta {
                        entry_sha256,
                        created_config,
                        created_agents_dir,
                        created_exclude,
                        exclude_separator_added,
                        created_mcp_servers,
                    }
                }
            };

            upsert_panel_row(
                &mut panels,
                &alias,
                app,
                &physical,
                RowMeta::Antigravity(agy_meta),
            );
            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                if config_backup.is_some() || exclude_backup.is_some() {
                    return rollback_project_panel_files(
                        spec,
                        want_json,
                        &alias,
                        &physical,
                        action_str,
                        (config_backup.as_ref(), exclude_backup.as_ref()),
                        record_err,
                    );
                }
                return Err(record_err);
            }

            if want_json {
                let report =
                    panel_action_json(app, &alias, &physical, PANEL_ENTRY_NAME, action_str, true);
                return Ok((0, json(&report)?, vec![]));
            }
            Ok((
                0,
                format!(
                    "{text_prefix} {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                    app.as_str(),
                    physical.display()
                )
                .into_bytes(),
                vec![],
            ))
        }
    }
}

enum CodexUninstallInspect {
    AlreadyGone,
    Edited,
    CleanRemove {
        remaining_bytes: Option<Vec<u8>>,
        prior_config: Option<(Vec<u8>, Stat)>,
    },
}

fn inspect_codex_config_for_uninstall(
    target_repo: &Path,
    expected_sha: &str,
    created_config: bool,
    separator_added: bool,
) -> Result<CodexUninstallInspect> {
    let repo_fd = open_dir_nofollow(target_repo, "project checkout")?;
    let Some(codex_fd) =
        open_existing_subdir_nofollow_at(&repo_fd, OsStr::new(".codex"), ".codex directory")?
    else {
        return Ok(CodexUninstallInspect::AlreadyGone);
    };
    let Some((raw_bytes, stat)) =
        read_regular_file_nofollow_at(&codex_fd, OsStr::new("config.toml"), ".codex/config.toml")?
    else {
        return Ok(CodexUninstallInspect::AlreadyGone);
    };

    let Ok(raw_str) = std::str::from_utf8(&raw_bytes) else {
        return Ok(CodexUninstallInspect::Edited);
    };
    let Ok(parsed_raw) = oms_toml::parse_bounded(&raw_bytes, MAX_CONFIG_BYTES) else {
        return Ok(CodexUninstallInspect::Edited);
    };
    let Ok(split) = split_codex_panel_block(raw_str) else {
        return Ok(CodexUninstallInspect::Edited);
    };

    match split.block {
        Some(on_disk_block) => {
            if oms_panel_core::digest(on_disk_block.as_bytes()) != expected_sha {
                return Ok(CodexUninstallInspect::Edited);
            }
            let before_trimmed = if separator_added
                && split.before.ends_with('\n')
                && (split.after.is_empty()
                    || split.after.starts_with('\n')
                    || split.after.starts_with("\r\n"))
            {
                &split.before[..split.before.len() - 1]
            } else {
                split.before
            };
            let remaining = format!("{before_trimmed}{}", split.after);
            let Ok(parsed_remaining) =
                oms_toml::parse_bounded(remaining.as_bytes(), MAX_CONFIG_BYTES)
            else {
                return Ok(CodexUninstallInspect::Edited);
            };
            let keep_empty_mcp = parsed_remaining.contains_key("mcp_servers");
            let Ok(stripped_raw) = table_without_oms_panel(parsed_raw, keep_empty_mcp) else {
                return Ok(CodexUninstallInspect::Edited);
            };
            if stripped_raw != parsed_remaining {
                return Ok(CodexUninstallInspect::Edited);
            }
            let remaining_bytes = if remaining.is_empty() && created_config {
                None
            } else {
                Some(remaining.into_bytes())
            };
            Ok(CodexUninstallInspect::CleanRemove {
                remaining_bytes,
                prior_config: Some((raw_bytes, stat)),
            })
        }
        None => {
            let has_panel_table = parsed_raw
                .get("mcp_servers")
                .and_then(oms_toml::Value::as_table)
                .is_some_and(|s| s.contains_key(PANEL_ENTRY_NAME));
            if has_panel_table {
                return Ok(CodexUninstallInspect::Edited);
            }
            if raw_bytes.is_empty() && created_config {
                Ok(CodexUninstallInspect::CleanRemove {
                    remaining_bytes: None,
                    prior_config: Some((raw_bytes, stat)),
                })
            } else {
                Ok(CodexUninstallInspect::AlreadyGone)
            }
        }
    }
}

fn inspect_antigravity_config_for_uninstall(
    target_repo: &Path,
    row: &PanelRecord,
) -> Result<CodexUninstallInspect> {
    let spec = ANTIGRAVITY_CONFIG_SPEC;
    let expected_sha = row.entry_sha256.as_deref().ok_or_else(|| {
        Error::new("Invalid OMS project panel map: missing Antigravity entry_sha256")
    })?;
    let created_config = row.created_config == Some(true);
    let created_mcp_servers = row.created_mcp_servers == Some(true);
    let drop_empty_mcp = created_config || created_mcp_servers;

    let repo_fd = open_dir_nofollow(target_repo, "project checkout")?;
    let Some(agents_fd) =
        open_existing_subdir_nofollow_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label)?
    else {
        return Ok(CodexUninstallInspect::AlreadyGone);
    };
    let Some((raw_bytes, stat)) =
        read_regular_file_nofollow_at(&agents_fd, OsStr::new(spec.file_name), spec.rel_path)?
    else {
        return Ok(CodexUninstallInspect::AlreadyGone);
    };

    let Ok(mut remaining_root) = parse_strict_json_object_bounded(&raw_bytes, spec.rel_path) else {
        return Ok(CodexUninstallInspect::Edited);
    };

    match remaining_root.get("mcpServers") {
        None => {
            if remaining_root.is_empty() && created_config {
                return Ok(CodexUninstallInspect::CleanRemove {
                    remaining_bytes: None,
                    prior_config: Some((raw_bytes, stat)),
                });
            }
            return Ok(CodexUninstallInspect::AlreadyGone);
        }
        Some(serde_json::Value::Object(servers)) => match servers.get(PANEL_ENTRY_NAME) {
            Some(entry_val) => {
                if !entry_val.is_object() {
                    return Ok(CodexUninstallInspect::Edited);
                }
                let Ok(on_disk_sha) = canonical_json_sha256(entry_val) else {
                    return Ok(CodexUninstallInspect::Edited);
                };
                if on_disk_sha != expected_sha {
                    return Ok(CodexUninstallInspect::Edited);
                }
            }
            None => {
                if !(servers.is_empty() && drop_empty_mcp) {
                    return Ok(CodexUninstallInspect::AlreadyGone);
                }
            }
        },
        Some(_) => return Ok(CodexUninstallInspect::Edited),
    }

    if let Some(serde_json::Value::Object(servers)) = remaining_root.get_mut("mcpServers") {
        servers.shift_remove(PANEL_ENTRY_NAME);
        if servers.is_empty() && drop_empty_mcp {
            remaining_root.shift_remove("mcpServers");
        }
    }

    if remaining_root.is_empty() && created_config {
        return Ok(CodexUninstallInspect::CleanRemove {
            remaining_bytes: None,
            prior_config: Some((raw_bytes, stat)),
        });
    }

    // A rewrite that fails to round-trip (over the size bound once pretty-printed,
    // or a value that does not survive re-encoding) is OMS's failure, not a user
    // edit: refuse before any change so the entry and its ownership row both stay.
    let bytes = encode_pretty_2space_json(&remaining_root)?;
    if parse_strict_json_object_bounded(&bytes, spec.rel_path).ok() != Some(remaining_root) {
        return Err(Error::new(format!(
            "cannot rewrite {} without changing its other entries; left it and the ownership record unchanged",
            spec.rel_path
        )));
    }

    Ok(CodexUninstallInspect::CleanRemove {
        remaining_bytes: Some(bytes),
        prior_config: Some((raw_bytes, stat)),
    })
}

pub(super) fn uninstall_panel(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let app = PanelApp::parse(&options.required("uninstall-panel")?)?;
    let alias = options.required("project")?;
    check_alias(&alias)?;
    let dry_run = options.has("dry-run");
    let want_json = options.has("json");

    let (target, explicit) = resolve_writer_path()?;
    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
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
        let Some(row) = recorded else {
            let report_path =
                std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
            if want_json {
                let report = panel_action_json(
                    app,
                    &alias,
                    &report_path,
                    PANEL_ENTRY_NAME,
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
        match app {
            PanelApp::Claude => {
                if !checkout_exists {
                    if want_json {
                        let report = panel_action_json(
                            app,
                            &alias,
                            &row.path,
                            PANEL_ENTRY_NAME,
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
                            row.path.display()
                        )
                        .into_bytes(),
                        vec![],
                    ));
                }
                let claude_bin = resolve_claude_program(cwd, true)?;
                if want_json {
                    let report = panel_action_json(
                        app,
                        &alias,
                        &row.path,
                        PANEL_ENTRY_NAME,
                        "uninstall",
                        false,
                    );
                    return Ok((0, json(&report)?, vec![]));
                }
                let mut text = format_vendor_command(
                    &row.path,
                    &claude_bin,
                    &["mcp", "get", PANEL_ENTRY_NAME],
                )?;
                text.push_str(&format_vendor_command(
                    &row.path,
                    &claude_bin,
                    &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                )?);
                return Ok((0, text.into_bytes(), vec![]));
            }
            PanelApp::Codex | PanelApp::Antigravity => {
                let spec = project_config_spec(app).expect("project config spec");
                let action = if checkout_exists {
                    "uninstall"
                } else {
                    "forgotten"
                };
                if checkout_exists {
                    let repo_fd = open_dir_nofollow(&row.path, "project checkout")?;
                    if let Some(sub_fd) = open_existing_subdir_nofollow_at(
                        &repo_fd,
                        OsStr::new(spec.dir_name),
                        spec.dir_label,
                    )? {
                        let _ = read_regular_file_nofollow_at(
                            &sub_fd,
                            OsStr::new(spec.file_name),
                            spec.rel_path,
                        )?;
                    }
                    if let GitConfigState::Tracked =
                        inspect_git_project_config(&row.path, spec.rel_path, stop)?
                    {
                        return Err(Error::new(format!(
                            "refusing to modify tracked file {} in {}; untrack it first",
                            spec.rel_path,
                            row.path.display()
                        )));
                    }
                }
                if want_json {
                    let report =
                        panel_action_json(app, &alias, &row.path, PANEL_ENTRY_NAME, action, false);
                    return Ok((0, json(&report)?, vec![]));
                }
                let msg = if checkout_exists {
                    format!(
                        "would uninstall {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                        app.as_str(),
                        row.path.display()
                    )
                } else {
                    format!(
                        "{} panel {PANEL_ENTRY_NAME} for {alias} ({}): checkout is gone; would drop the record\n",
                        app.as_str(),
                        row.path.display()
                    )
                };
                return Ok((0, msg.into_bytes(), vec![]));
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
            row.scope == app.expected_scope() && row.name == PANEL_ENTRY_NAME,
            "Invalid OMS project panel map: unexpected scope or entry name",
        )?;
    }

    let Some(row) = recorded else {
        let report_path = std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
        if want_json {
            let report = panel_action_json(
                app,
                &alias,
                &report_path,
                PANEL_ENTRY_NAME,
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
                panel_action_json(app, &alias, &row.path, PANEL_ENTRY_NAME, "forgotten", true);
            return Ok((0, json(&report)?, vec![]));
        }
        let detail = match app {
            PanelApp::Claude => {
                "dropped the record (the Claude entry stored for that path is inert)"
            }
            PanelApp::Codex | PanelApp::Antigravity => "dropped the record",
        };
        return Ok((
            0,
            format!(
                "{} panel {PANEL_ENTRY_NAME} for {alias} ({}): checkout is gone; {detail}\n",
                app.as_str(),
                row.path.display()
            )
            .into_bytes(),
            vec![],
        ));
    }

    match app {
        PanelApp::Claude => {
            enum ClaudeUninstallDisposition {
                Removed,
                AlreadyGone,
                LeftModified,
            }
            let launcher = resolve_oms_launcher_path()?;
            let claude_bin = resolve_claude_program(cwd, false)?;
            let disposition = match probe_claude_entry(&claude_bin, &row.path, stop)? {
                Some(found) if found.scope == Some(ClaudeScope::Local) => {
                    if found.matches_expected(&launcher, &row.path) {
                        let out = run_claude_command(
                            &claude_bin,
                            &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                            &row.path,
                            stop,
                        )
                        .map_err(VendorExecError::into_error)?;
                        if out.code == Some(0) {
                            ClaudeUninstallDisposition::Removed
                        } else if is_mcp_entry_missing(&out) {
                            ClaudeUninstallDisposition::AlreadyGone
                        } else {
                            return Err(vendor_failure("claude mcp", "remove", &out));
                        }
                    } else {
                        ClaudeUninstallDisposition::LeftModified
                    }
                }
                Some(_) | None => ClaudeUninstallDisposition::AlreadyGone,
            };
            drop_panel_row(&mut panels, &alias, app);
            write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
            if want_json {
                let report =
                    panel_action_json(app, &alias, &row.path, PANEL_ENTRY_NAME, "uninstall", true);
                return Ok((0, json(&report)?, vec![]));
            }
            let msg = match disposition {
                ClaudeUninstallDisposition::Removed => format!(
                    "uninstalled {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                    app.as_str(),
                    row.path.display()
                ),
                ClaudeUninstallDisposition::AlreadyGone => format!(
                    "{} panel {PANEL_ENTRY_NAME} for {alias} was already gone; dropped ownership record ({})\n",
                    app.as_str(),
                    row.path.display()
                ),
                ClaudeUninstallDisposition::LeftModified => format!(
                    "the local {PANEL_ENTRY_NAME} entry was changed outside OMS; left it in place and dropped ownership record for {alias} ({})\n",
                    row.path.display()
                ),
            };
            Ok((0, msg.into_bytes(), vec![]))
        }
        PanelApp::Codex | PanelApp::Antigravity => {
            let spec = project_config_spec(app).expect("project config spec");
            let (inspect, created_subdir, edited_subject) = match app {
                PanelApp::Codex => {
                    let expected_sha = row.block_sha256.as_deref().ok_or_else(|| {
                        Error::new("Invalid OMS project panel map: missing Codex block_sha256")
                    })?;
                    let inspect = inspect_codex_config_for_uninstall(
                        &row.path,
                        expected_sha,
                        row.created_config == Some(true),
                        row.separator_added == Some(true),
                    )?;
                    (inspect, row.created_codex_dir == Some(true), "OMS block")
                }
                PanelApp::Antigravity => {
                    let inspect = inspect_antigravity_config_for_uninstall(&row.path, &row)?;
                    (
                        inspect,
                        row.created_agents_dir == Some(true),
                        "mcpServers.oms-panel entry",
                    )
                }
                PanelApp::Claude => unreachable!(),
            };
            let created_exclude = row.created_exclude == Some(true);
            let exclude_separator_added = row.exclude_separator_added == Some(true);

            let git_state = inspect_git_project_config(&row.path, spec.rel_path, stop)?;
            if matches!(git_state, GitConfigState::Tracked) {
                return Err(Error::new(format!(
                    "refusing to modify tracked file {} in {}; untrack it first",
                    spec.rel_path,
                    row.path.display()
                )));
            }
            let config_path = row.path.join(spec.rel_path);

            if matches!(inspect, CodexUninstallInspect::Edited) {
                drop_panel_row(&mut panels, &alias, app);
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
                if want_json {
                    let mut report = panel_action_json(
                        app,
                        &alias,
                        &row.path,
                        PANEL_ENTRY_NAME,
                        "uninstall",
                        true,
                    );
                    report["preservedFiles"] = json!([config_path]);
                    return Ok((0, json(&report)?, vec![]));
                }
                return Ok((
                    0,
                    format!(
                        "the {edited_subject} in {} was edited outside OMS; left it in place and dropped ownership record for {alias}\n",
                        config_path.display()
                    )
                    .into_bytes(),
                    vec![],
                ));
            }

            let mut config_backup: Option<ConfigBackup> = None;
            let mut removed_config_or_entry = false;

            if let CodexUninstallInspect::CleanRemove {
                remaining_bytes,
                prior_config,
            } = inspect
            {
                let (removed, backup) = apply_uninstall_config_clean_remove(
                    spec,
                    &row.path,
                    remaining_bytes,
                    prior_config,
                )?;
                removed_config_or_entry = removed;
                config_backup = backup;
            }

            if created_subdir {
                let repo_fd = open_dir_nofollow(&row.path, "project checkout")?;
                if let Err(e) =
                    try_rmdir_if_empty_at(&repo_fd, OsStr::new(spec.dir_name), spec.dir_label)
                {
                    if config_backup.is_some() {
                        return rollback_project_panel_files(
                            spec,
                            want_json,
                            &alias,
                            &row.path,
                            "uninstall",
                            (config_backup.as_ref(), None),
                            e,
                        );
                    }
                    return Err(e);
                }
            }

            let exclude_backup = match apply_uninstall_exclude_step(
                &mut panels,
                spec,
                &alias,
                git_state,
                created_exclude,
                exclude_separator_added,
                stop,
            ) {
                Ok(backup) => backup,
                Err(ex_err) => {
                    if config_backup.is_some() {
                        return rollback_project_panel_files(
                            spec,
                            want_json,
                            &alias,
                            &row.path,
                            "uninstall",
                            (config_backup.as_ref(), None),
                            ex_err,
                        );
                    }
                    return Err(ex_err);
                }
            };

            drop_panel_row(&mut panels, &alias, app);
            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                if config_backup.is_some() || exclude_backup.is_some() {
                    return rollback_project_panel_files(
                        spec,
                        want_json,
                        &alias,
                        &row.path,
                        "uninstall",
                        (config_backup.as_ref(), exclude_backup.as_ref()),
                        record_err,
                    );
                }
                return Err(record_err);
            }

            if want_json {
                let report =
                    panel_action_json(app, &alias, &row.path, PANEL_ENTRY_NAME, "uninstall", true);
                return Ok((0, json(&report)?, vec![]));
            }
            let msg = if removed_config_or_entry {
                format!(
                    "uninstalled {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
                    app.as_str(),
                    row.path.display()
                )
            } else {
                format!(
                    "{} panel {PANEL_ENTRY_NAME} for {alias} was already gone; dropped ownership record ({})\n",
                    app.as_str(),
                    row.path.display()
                )
            };
            Ok((0, msg.into_bytes(), vec![]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AntigravityRowMeta, ClaudeScope, CmdOutput, CodexRowMeta, CodexTrustStatus,
        OMS_PACKAGE_VERSION, PanelApp, PanelMap, PanelRecord, RowMeta, attach_codex_trust_fields,
        canonical_json_sha256, check_codex_project_trust_at, check_codex_project_trust_with_env,
        claude_panel_entry_json, codex_panel_block, codex_panel_block_for_versions,
        codex_panel_resource_version, encode_panels_json, encode_pretty_2space_json,
        expected_antigravity_panel_entry, expected_antigravity_panel_entry_for_version,
        expected_codex_panel_server_table, format_codex_install_text, is_mcp_entry_missing,
        parse_claude_mcp_get, parse_panels_json, parse_strict_json_object_bounded,
        resolve_codex_trust_in_toml, split_codex_panel_block, upsert_panel_row,
    };
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

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
    fn codex_block_rendering_toml_parsing_and_trust_checks() {
        assert!(!OMS_PACKAGE_VERSION.is_empty());
        assert!(!codex_panel_resource_version().is_empty());

        let launcher = Path::new("/tmp/runtime-owner/.local/bin/oms");
        let repo = Path::new("/srv/checkouts/PoseFixer");
        let block = codex_panel_block(launcher, repo).unwrap();
        let split = split_codex_panel_block(&block).unwrap();
        assert_eq!(split.before, "");
        assert_eq!(split.block, Some(block.as_str()));
        assert_eq!(split.after, "");

        let parsed = oms_toml::parse_str(&block).unwrap();
        let expected =
            expected_codex_panel_server_table(launcher, repo, codex_panel_resource_version())
                .unwrap();
        assert_eq!(
            parsed
                .get("mcp_servers")
                .and_then(oms_toml::Value::as_table)
                .and_then(|s| s.get("oms-panel")),
            Some(&oms_toml::Value::Table(expected))
        );

        let older = codex_panel_block_for_versions(launcher, repo, "0.7.0-old", "v0").unwrap();
        assert_ne!(older, block);
        assert!(oms_toml::parse_str(&older).is_ok());

        for bad_framing in [
            "# BEGIN oh-my-setting codex-panel\n[mcp_servers.oms-panel]\n",
            "# END oh-my-setting codex-panel\n",
            "# BEGIN oh-my-setting codex-panel\n# END oh-my-setting codex-panel\n# BEGIN oh-my-setting codex-panel\n# END oh-my-setting codex-panel\n",
        ] {
            assert!(
                split_codex_panel_block(bad_framing).is_err(),
                "accepted broken framing: {bad_framing:?}"
            );
        }

        let cfg_label = Path::new("/tmp/codex-home/config.toml");
        let sub = Path::new("/srv/checkouts/PoseFixer/sub");
        let wt = Path::new("/srv/worktrees/PoseFixer-wt");
        let trusted_toml = br#"[projects."/srv/checkouts/PoseFixer"]
trust_level = "trusted"
"#;
        assert_eq!(
            resolve_codex_trust_in_toml(trusted_toml, repo, Some(repo), cfg_label),
            CodexTrustStatus::Trusted
        );
        assert_eq!(
            resolve_codex_trust_in_toml(trusted_toml, sub, Some(repo), cfg_label),
            CodexTrustStatus::Trusted
        );
        assert_eq!(
            resolve_codex_trust_in_toml(trusted_toml, wt, Some(repo), cfg_label),
            CodexTrustStatus::Trusted
        );
        assert_eq!(
            resolve_codex_trust_in_toml(
                trusted_toml,
                Path::new("/srv/checkouts/other"),
                Some(Path::new("/srv/checkouts/other")),
                cfg_label,
            ),
            CodexTrustStatus::Untrusted {
                trust_target: Path::new("/srv/checkouts/other").to_path_buf(),
            }
        );

        // Explicit non-trusted value on worktree wins over trusted repo root, and hint names worktree.
        let wt_untrusted = br#"[projects."/srv/checkouts/PoseFixer"]
trust_level = "trusted"

[projects."/srv/worktrees/PoseFixer-wt"]
trust_level = "untrusted"
"#;
        assert_eq!(
            resolve_codex_trust_in_toml(wt_untrusted, wt, Some(repo), cfg_label),
            CodexTrustStatus::Untrusted {
                trust_target: wt.to_path_buf(),
            }
        );

        // Worktree table without `trust_level` falls back to trusted repo root.
        let wt_without_trust_level = br#"[projects."/srv/checkouts/PoseFixer"]
trust_level = "trusted"

[projects."/srv/worktrees/PoseFixer-wt"]
note = "no trust_level"
"#;
        assert_eq!(
            resolve_codex_trust_in_toml(wt_without_trust_level, wt, Some(repo), cfg_label),
            CodexTrustStatus::Trusted
        );

        // Missing both CODEX_HOME and HOME reports codex-trust-unknown, not codex-project-untrusted.
        for (codex_env, home_env) in [
            (None, None),
            (Some(OsStr::new("")), None),
            (None, Some(OsStr::new(""))),
            (Some(OsStr::new("")), Some(OsStr::new(""))),
        ] {
            let status =
                check_codex_project_trust_with_env(repo, repo, Some(repo), codex_env, home_env);
            assert!(
                matches!(
                    status,
                    CodexTrustStatus::Unknown { ref hint } if !hint.is_empty() && !hint.contains('\n')
                ),
                "expected Unknown for ({codex_env:?}, {home_env:?}), got {status:?}"
            );
            let mut report = serde_json::json!({});
            attach_codex_trust_fields(&mut report, &status);
            assert_eq!(report["active"], false);
            assert_eq!(report["reason"], "codex-trust-unknown");
            let plain = String::from_utf8(format_codex_install_text(
                "PoseFixer",
                repo,
                "installed",
                &status,
            ))
            .unwrap();
            assert!(
                plain.contains("(inactive: codex-trust-unknown)\nhint: ")
                    && !plain.contains("codex-project-untrusted"),
                "{plain}"
            );
        }

        // Non-repo ancestor never counts.
        let parent_trusted = br#"[projects."/srv/checkouts"]
trust_level = "trusted"
"#;
        assert_eq!(
            resolve_codex_trust_in_toml(parent_trusted, repo, Some(repo), cfg_label),
            CodexTrustStatus::Untrusted {
                trust_target: repo.to_path_buf(),
            }
        );
        assert_eq!(
            resolve_codex_trust_in_toml(parent_trusted, repo, None, cfg_label),
            CodexTrustStatus::Untrusted {
                trust_target: repo.to_path_buf(),
            }
        );

        // Invalid TOML reports Unknown rather than Untrusted.
        assert!(matches!(
            resolve_codex_trust_in_toml(b"[broken\n", repo, Some(repo), cfg_label),
            CodexTrustStatus::Unknown { .. }
        ));

        // Foreign UID on CODEX_HOME or config.toml reports Unknown rather than failing.
        let tmp = std::env::temp_dir().join(format!("oms-trust-uid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.toml"), trusted_toml).unwrap();
        let euid = rustix::process::geteuid().as_raw();
        assert_eq!(
            check_codex_project_trust_at(&tmp, repo, Some(repo), euid),
            CodexTrustStatus::Trusted
        );
        assert!(matches!(
            check_codex_project_trust_at(&tmp, repo, Some(repo), euid.wrapping_add(1)),
            CodexTrustStatus::Unknown { ref hint } if hint.contains("not owned by the current user")
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn antigravity_entry_json_canonical_sha_and_key_order() {
        let launcher = Path::new("/tmp/runtime-owner/.local/bin/oms");
        let repo = Path::new("/srv/checkouts/PoseFixer");
        let entry = expected_antigravity_panel_entry(launcher, repo).unwrap();
        assert_eq!(entry["command"], "/tmp/runtime-owner/.local/bin/oms");
        assert_eq!(entry["args"], serde_json::json!(["mcp-server"]));
        assert_eq!(entry["env"]["OMS_MCP_TOOL_PROFILE"], "chatgpt-panel");
        assert_eq!(entry["env"]["OMS_PANEL_REPO"], "/srv/checkouts/PoseFixer");
        assert_eq!(
            entry["env"]["OMS_MCP_PANEL_RESOURCE_VERSION"],
            codex_panel_resource_version()
        );
        assert!(entry.get("type").is_none());

        let reordered = serde_json::json!({
            "env": {
                "OMS_PANEL_REPO": "/srv/checkouts/PoseFixer",
                "OMS_MCP_PANEL_RESOURCE_VERSION": codex_panel_resource_version(),
                "OMS_MCP_TOOL_PROFILE": "chatgpt-panel",
            },
            "args": ["mcp-server"],
            "command": "/tmp/runtime-owner/.local/bin/oms",
        });
        assert_eq!(
            canonical_json_sha256(&entry).unwrap(),
            canonical_json_sha256(&reordered).unwrap()
        );
        let older =
            expected_antigravity_panel_entry_for_version(launcher, repo, "v0-older").unwrap();
        assert_ne!(
            canonical_json_sha256(&entry).unwrap(),
            canonical_json_sha256(&older).unwrap()
        );

        // 2-space pretty JSON round-trips byte-exact and shift_remove preserves key order.
        let pretty_sample = b"{\n  \"zetaTop\": true,\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"a-bin\"\n    },\n    \"oms-panel\": {\n      \"command\": \"oms\"\n    },\n    \"b\": {\n      \"command\": \"b-bin\"\n    },\n    \"c\": {\n      \"command\": \"c-bin\"\n    }\n  },\n  \"alphaTail\": 1\n}\n";
        let mut parsed = parse_strict_json_object_bounded(pretty_sample, "sample").unwrap();
        assert_eq!(
            encode_pretty_2space_json(&parsed).unwrap().as_slice(),
            pretty_sample
        );
        parsed
            .get_mut("mcpServers")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .shift_remove("oms-panel");
        let server_keys: Vec<&str> = parsed["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(server_keys, vec!["a", "b", "c"]);

        // Duplicate keys in JSON config are rejected.
        assert!(
            parse_strict_json_object_bounded(
                br#"{"mcpServers":{},"mcpServers":{}}"#,
                ".agents/mcp_config.json"
            )
            .is_err()
        );
    }

    #[test]
    fn panel_app_and_sidecar_validation_stay_strict() {
        assert_eq!(
            oms_scrub::scan(include_bytes!("project_panels.rs"), &[]),
            Ok(Vec::new())
        );
        assert_eq!(PanelApp::parse("claude").unwrap(), PanelApp::Claude);
        assert_eq!(PanelApp::parse("codex").unwrap(), PanelApp::Codex);
        assert_eq!(
            PanelApp::parse("antigravity").unwrap(),
            PanelApp::Antigravity
        );
        let err = PanelApp::parse("agy").unwrap_err();
        assert!(
            err.message
                .contains("supported: claude, codex, antigravity"),
            "{}",
            err.message
        );

        let sha = "a".repeat(64);
        let ok = format!(
            r#"{{"PoseFixer":[{{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/PoseFixer"}},{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/PoseFixer","block_sha256":"{sha}","created_config":false,"created_codex_dir":false,"created_exclude":true,"separator_added":true,"exclude_separator_added":true}},{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/PoseFixer","entry_sha256":"{sha}","created_config":true,"created_agents_dir":true,"created_exclude":true}}]}}"#
        );
        assert_eq!(parse_panels_json(&ok).unwrap()["PoseFixer"].len(), 3);

        let claude_with_codex_fields = format!(
            r#"{{"alpha":[{{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":false,"created_codex_dir":false,"created_exclude":false}}]}}"#
        );
        let claude_with_agy_fields = format!(
            r#"{{"alpha":[{{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}"}}]}}"#
        );
        let claude_with_sep_field = r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha","separator_added":true}]}"#;
        let codex_old_plugin_shape = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"user","name":"oms-alpha@oms-host-panels","path":"/srv/alpha","plugin_sha256":"{sha}","mcp_sha256":"{sha}"}}]}}"#
        );
        let codex_upper_hash = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{}","created_config":true,"created_codex_dir":true,"created_exclude":true}}]}}"#,
            "A".repeat(64)
        );
        let codex_missing_flags = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}"}}]}}"#
        );
        let codex_dir_without_file = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":false,"created_codex_dir":true,"created_exclude":false}}]}}"#
        );
        let codex_with_agy_field = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":true,"created_codex_dir":true,"created_exclude":false,"created_agents_dir":false}}]}}"#
        );
        let agy_with_codex_field = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","block_sha256":"{sha}","created_config":true,"created_agents_dir":true,"created_exclude":false}}]}}"#
        );
        let agy_dir_without_file = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":false,"created_agents_dir":true,"created_exclude":false}}]}}"#
        );
        let agy_excl_sep_without_excl = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":true,"created_agents_dir":true,"created_exclude":false,"exclude_separator_added":true}}]}}"#
        );
        let agy_duplicate_checkout_aliases = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":true,"created_agents_dir":true,"created_exclude":true}}],"beta":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":false,"created_agents_dir":false,"created_exclude":false}}]}}"#
        );
        let agy_removed_prior_ws = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":false,"created_agents_dir":false,"created_exclude":false,"prior_ws":",,"}}]}}"#
        );
        let agy_removed_prior_sha = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":false,"created_agents_dir":false,"created_exclude":false,"prior_sha256":"{sha}"}}]}}"#
        );
        let agy_with_separator_added = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":false,"created_agents_dir":false,"created_exclude":false,"separator_added":true}}]}}"#
        );
        let agy_mcp_servers_with_created_config = format!(
            r#"{{"alpha":[{{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"{sha}","created_config":true,"created_agents_dir":true,"created_exclude":false,"created_mcp_servers":true}}]}}"#
        );
        let codex_sep_with_created_file = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":true,"created_codex_dir":true,"created_exclude":false,"separator_added":true}}]}}"#
        );
        assert_eq!(
            parse_panels_json(&codex_sep_with_created_file).unwrap()["alpha"].len(),
            1
        );

        // Every row shape the install paths can produce round-trips through encode then parse.
        let mut claude_only = PanelMap::new();
        upsert_panel_row(
            &mut claude_only,
            "alpha",
            PanelApp::Claude,
            Path::new("/srv/alpha"),
            RowMeta::Claude,
        );
        let claude_bytes = encode_panels_json(&claude_only).unwrap();
        assert_eq!(
            parse_panels_json(std::str::from_utf8(&claude_bytes).unwrap()).unwrap(),
            claude_only
        );

        for (created_config, created_codex_dir) in [(true, true), (true, false), (false, false)] {
            for separator_added in [false, true] {
                for (created_exclude, exclude_separator_added) in
                    [(false, false), (true, false), (true, true)]
                {
                    let meta = CodexRowMeta {
                        block_sha256: sha.clone(),
                        created_config,
                        created_codex_dir,
                        created_exclude,
                        separator_added,
                        exclude_separator_added,
                    };
                    let mut single = PanelMap::new();
                    upsert_panel_row(
                        &mut single,
                        "PoseFixer",
                        PanelApp::Codex,
                        Path::new("/srv/PoseFixer"),
                        RowMeta::Codex(meta.clone()),
                    );
                    let encoded = encode_panels_json(&single).unwrap();
                    let round_tripped =
                        parse_panels_json(std::str::from_utf8(&encoded).unwrap()).unwrap();
                    assert_eq!(round_tripped, single, "round-trip mismatch for {meta:?}");

                    let agy_meta = AntigravityRowMeta {
                        entry_sha256: sha.clone(),
                        created_config,
                        created_agents_dir: created_codex_dir,
                        created_exclude,
                        exclude_separator_added,
                        created_mcp_servers: !created_config && separator_added,
                    };
                    let mut trio = PanelMap::new();
                    upsert_panel_row(
                        &mut trio,
                        "PoseFixer",
                        PanelApp::Claude,
                        Path::new("/srv/PoseFixer"),
                        RowMeta::Claude,
                    );
                    upsert_panel_row(
                        &mut trio,
                        "PoseFixer",
                        PanelApp::Codex,
                        Path::new("/srv/PoseFixer"),
                        RowMeta::Codex(meta),
                    );
                    upsert_panel_row(
                        &mut trio,
                        "PoseFixer",
                        PanelApp::Antigravity,
                        Path::new("/srv/PoseFixer"),
                        RowMeta::Antigravity(agy_meta),
                    );
                    let trio_bytes = encode_panels_json(&trio).unwrap();
                    assert_eq!(
                        parse_panels_json(std::str::from_utf8(&trio_bytes).unwrap()).unwrap(),
                        trio
                    );
                }
            }
        }

        let codex_excl_sep_without_excl = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":true,"created_codex_dir":true,"created_exclude":false,"exclude_separator_added":true}}]}}"#
        );
        let codex_duplicate_checkout_aliases = format!(
            r#"{{"alpha":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":true,"created_codex_dir":true,"created_exclude":true}}],"beta":[{{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"{sha}","created_config":false,"created_codex_dir":false,"created_exclude":false}}]}}"#
        );
        for bad in [
            r#"{"alpha":[]}"#,
            claude_with_codex_fields.as_str(),
            claude_with_agy_fields.as_str(),
            claude_with_sep_field,
            codex_old_plugin_shape.as_str(),
            codex_upper_hash.as_str(),
            codex_missing_flags.as_str(),
            codex_dir_without_file.as_str(),
            codex_with_agy_field.as_str(),
            agy_with_codex_field.as_str(),
            agy_dir_without_file.as_str(),
            agy_excl_sep_without_excl.as_str(),
            agy_duplicate_checkout_aliases.as_str(),
            agy_removed_prior_ws.as_str(),
            agy_removed_prior_sha.as_str(),
            agy_with_separator_added.as_str(),
            agy_mcp_servers_with_created_config.as_str(),
            codex_excl_sep_without_excl.as_str(),
            codex_duplicate_checkout_aliases.as_str(),
            r#"{"alpha":[{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"antigravity","scope":"local","name":"oms-panel","path":"/srv/alpha","entry_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","created_config":true,"created_agents_dir":true,"created_exclude":true}]}"#,
            r#"{"alpha":[{"app":"codex","scope":"user","name":"oms-panel","path":"/srv/alpha"}]}"#,
            r#"{"alpha":[{"app":"codex","scope":"project","name":"oms-panel","path":"/srv/alpha","block_sha256":"short","created_config":true,"created_codex_dir":true,"created_exclude":true}]}"#,
            r#"{"alpha":[{"app":"antigravity","scope":"project","name":"oms-panel","path":"/srv/alpha","entry_sha256":"short","created_config":true,"created_agents_dir":true,"created_exclude":true}]}"#,
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

        // Defense in depth: encode_panels_json refuses invalid maps before writing.
        let base_codex = || PanelRecord {
            app: "codex".into(),
            scope: "project".into(),
            name: "oms-panel".into(),
            path: PathBuf::from("/srv/alpha"),
            block_sha256: Some(sha.clone()),
            entry_sha256: None,
            created_config: Some(true),
            created_codex_dir: Some(true),
            created_agents_dir: None,
            created_exclude: Some(false),
            separator_added: None,
            exclude_separator_added: None,
            created_mcp_servers: None,
        };
        let base_agy = || PanelRecord {
            app: "antigravity".into(),
            scope: "project".into(),
            name: "oms-panel".into(),
            path: PathBuf::from("/srv/alpha"),
            block_sha256: None,
            entry_sha256: Some(sha.clone()),
            created_config: Some(true),
            created_codex_dir: None,
            created_agents_dir: Some(true),
            created_exclude: Some(false),
            separator_added: None,
            exclude_separator_added: None,
            created_mcp_servers: None,
        };
        let invalid_maps: Vec<PanelMap> = vec![
            BTreeMap::new(),
            BTreeMap::from([("alpha".into(), vec![])]),
            BTreeMap::from([("-bad-alias".into(), vec![base_codex()])]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    created_config: Some(false),
                    created_codex_dir: Some(true),
                    ..base_codex()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    created_config: Some(false),
                    created_agents_dir: Some(true),
                    ..base_agy()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    created_exclude: Some(false),
                    exclude_separator_added: Some(true),
                    ..base_codex()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    created_exclude: Some(false),
                    exclude_separator_added: Some(true),
                    ..base_agy()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    block_sha256: Some("short".into()),
                    ..base_codex()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    entry_sha256: Some("short".into()),
                    ..base_agy()
                }],
            )]),
            BTreeMap::from([(
                "alpha".into(),
                vec![PanelRecord {
                    app: "claude".into(),
                    scope: "local".into(),
                    ..base_codex()
                }],
            )]),
            BTreeMap::from([
                ("alpha".into(), vec![base_codex()]),
                ("beta".into(), vec![base_codex()]),
            ]),
            BTreeMap::from([
                ("alpha".into(), vec![base_agy()]),
                ("beta".into(), vec![base_agy()]),
            ]),
        ];
        for bad_map in &invalid_maps {
            assert!(
                encode_panels_json(bad_map).is_err(),
                "encode_panels_json accepted invalid map: {bad_map:?}"
            );
        }
    }
}
