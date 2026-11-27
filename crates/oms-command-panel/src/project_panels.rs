use crate::{Error, Output, Result, cli::Options, json, parent_only, render, require};
use rustix::fs::{AtFlags, Stat};
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{
    INVALID_MAP_MSG, MAX_BYTES, MAX_ENTRIES, TargetKind, check_alias, check_destination_unchanged,
    lock_map_file, open_parent_dir, physical_parent_and_leaf, read_map_at, read_private_sidecar_at,
    resolve_writer_path, sync_dir, valid_alias, validate_checkout, validate_map_stat,
    validate_private_sidecar_stat, write_atomic,
};

pub(crate) const PANELS_FILE_NAME: &str = "desktop-project-panels.json";
const PANEL_ENTRY_NAME: &str = "oms-panel";
const LIFECYCLE_OWNED_KEY: &str = "oh-my-setting";
const VENDOR_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PanelApp {
    Claude,
}

impl PanelApp {
    pub(crate) const SUPPORTED: &'static str = "claude";

    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw {
            "claude" => Ok(Self::Claude),
            _ => Err(Error::new(format!(
                "unsupported panel app '{raw}' (supported: {})",
                Self::SUPPORTED
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }

    const fn expected_scope(self) -> &'static str {
        match self {
            Self::Claude => "local",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PanelRecord {
    pub app: String,
    pub scope: String,
    pub name: String,
    pub path: PathBuf,
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
        let mut seen = std::collections::BTreeSet::new();
        for row in &rows {
            let app = PanelApp::parse(&row.app).map_err(|_| {
                Error::new("Invalid OMS project panel map: unsupported panel app in record")
            })?;
            require(
                row.scope == app.expected_scope()
                    && row.name == PANEL_ENTRY_NAME
                    && row.name != LIFECYCLE_OWNED_KEY
                    && normalized_abs_utf8(&row.path)
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
            rustix::fs::unlinkat(dir_fd, name, AtFlags::empty()).map_err(|e| Error {
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
        && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
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
    if let Some(raw) = std::env::var_os("OMS_CLAUDE_BIN").filter(|v| !v.is_empty()) {
        require(
            raw.to_str().is_some() && !raw.as_bytes().contains(&0),
            "OMS_CLAUDE_BIN must be valid UTF-8",
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
                    "Claude CLI '{}' from OMS_CLAUDE_BIN was not found on PATH",
                    candidate.display()
                ))
            })?
        };
        if !dry_run && !is_executable(&resolved) {
            return Err(Error::new(format!(
                "Claude CLI '{}' from OMS_CLAUDE_BIN is not an executable file",
                resolved.display()
            )));
        }
        return Ok(resolved);
    }
    if dry_run {
        return Ok(PathBuf::from("claude"));
    }
    find_on_path(Path::new("claude")).ok_or_else(|| {
        Error::new("Claude CLI 'claude' was not found on PATH (or set OMS_CLAUDE_BIN)")
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

fn read_bounded_pipe(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = (&mut pipe)
        .take(MAX_CAPTURE_BYTES as u64)
        .read_to_end(&mut buf);
    let _ = std::io::copy(&mut pipe, &mut std::io::sink());
    String::from_utf8_lossy(&buf).into_owned()
}

fn run_bounded_command(
    program: &Path,
    args: &[&str],
    workdir: &Path,
    stop: &AtomicBool,
) -> Result<CmdOutput> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| Error::new(format!("cannot execute Claude CLI: {e}")))?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("missing child stdout pipe"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| Error::new("missing child stderr pipe"))?;

    let pid = i32::try_from(child.id())
        .ok()
        .filter(|p| *p > 0)
        .and_then(Pid::from_raw);

    std::thread::scope(|scope| {
        let out_reader = scope.spawn(|| read_bounded_pipe(stdout_pipe));
        let err_reader = scope.spawn(|| read_bounded_pipe(stderr_pipe));
        let deadline = Instant::now() + VENDOR_TIMEOUT;
        let status = loop {
            if stop.load(Ordering::Acquire) {
                if let Some(pgid) = pid {
                    let _ = kill_process_group(pgid, Signal::KILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error {
                    code: 130,
                    message: "interrupted while running Claude CLI".into(),
                    publication_uncertain: false,
                });
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        if let Some(pgid) = pid {
                            let _ = kill_process_group(pgid, Signal::KILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(Error::new("Claude CLI timed out after 30s"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    if let Some(pgid) = pid {
                        let _ = kill_process_group(pgid, Signal::KILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::new(format!("failed waiting for Claude CLI: {e}")));
                }
            }
        };
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        Ok(CmdOutput {
            code: status.code(),
            stdout,
            stderr,
        })
    })
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

fn vendor_failure(subcommand: &str, out: &CmdOutput) -> Error {
    let status = out
        .code
        .map_or_else(|| "terminated by signal".into(), |c| format!("exit {c}"));
    match sanitized_vendor_reason(out) {
        Some(reason) => Error::new(format!(
            "claude mcp {subcommand} failed ({status}): {reason}"
        )),
        None => Error::new(format!("claude mcp {subcommand} failed ({status})")),
    }
}

fn is_mcp_entry_missing(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "not found",
        "no mcp server",
        "no local mcp server",
        "no project-scoped mcp server",
        "does not exist",
        "already gone",
        "already removed",
        "no server named",
        "unknown server",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
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
}

impl ClaudeMcpEntry {
    fn matches_expected(&self, launcher: &Path, physical_repo: &Path) -> bool {
        let Some(launcher_str) = launcher.to_str() else {
            return false;
        };
        let Some(repo_str) = physical_repo.to_str() else {
            return false;
        };
        self.scope == Some(ClaudeScope::Local)
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
    if trimmed.starts_with('{')
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let cfg = val.get("config").unwrap_or(&val);
        let scope = val
            .get("scope")
            .or_else(|| cfg.get("scope"))
            .and_then(|v| v.as_str())
            .and_then(classify_scope_token)?;
        let transport_type = cfg.get("type").and_then(|v| v.as_str()).map(str::to_owned);
        let command = cfg
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let args = cfg
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        if let Some(obj) = cfg.get("env").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                env.insert(k.clone(), v.as_str()?.to_owned());
            }
        }
        return Some(ClaudeMcpEntry {
            scope: Some(scope),
            transport_type,
            command,
            args,
            env,
        });
    }

    let mut entry = ClaudeMcpEntry::default();
    let mut in_env = false;
    let mut env_indent = 0usize;
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
            if let Some((k, v)) = body.split_once('=') {
                entry.env.insert(k.trim().to_owned(), v.to_owned());
                continue;
            }
            in_env = false;
        } else {
            in_env = false;
        }

        if let Some((label, rest)) = body.split_once(':') {
            let key = label.trim().to_ascii_lowercase();
            let value = rest.trim();
            match key.as_str() {
                "scope" => entry.scope = classify_scope_token(value),
                "type" => entry.transport_type = Some(value.to_owned()),
                "command" => entry.command = Some(value.to_owned()),
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
    entry.scope.is_some().then_some(entry)
}

fn probe_claude_entry(
    claude_bin: &Path,
    workdir: &Path,
    stop: &AtomicBool,
) -> Result<Option<ClaudeMcpEntry>> {
    let out = run_bounded_command(claude_bin, &["mcp", "get", PANEL_ENTRY_NAME], workdir, stop)?;
    if out.code == Some(0) {
        if let Some(parsed) = parse_claude_mcp_get(&out.stdout) {
            return Ok(Some(parsed));
        }
        if is_mcp_entry_missing(&out.stdout, &out.stderr) {
            return Ok(None);
        }
        return Err(Error::new(
            "cannot determine scope from `claude mcp get oms-panel` output",
        ));
    }
    if is_mcp_entry_missing(&out.stdout, &out.stderr) {
        return Ok(None);
    }
    Err(vendor_failure("get", &out))
}

fn panel_action_json(
    app: PanelApp,
    alias: &str,
    path: &Path,
    action: &str,
    changed: bool,
) -> serde_json::Value {
    json!({
        "app": app.as_str(),
        "alias": alias,
        "path": path,
        "entry": PANEL_ENTRY_NAME,
        "action": action,
        "changed": changed,
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
    let launcher = resolve_oms_launcher_path()?;
    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
    let Some(dir_fd) = open_parent_dir(&parent, false)? else {
        return Err(if explicit {
            Error::new(INVALID_MAP_MSG)
        } else {
            unknown_alias_error(&alias)
        });
    };
    if let Ok(stat) = rustix::fs::statat(&dir_fd, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
        validate_map_stat(&stat)?;
    }
    if let Ok(stat) = rustix::fs::statat(
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
                let claude_bin = resolve_claude_program(cwd, true)?;
                let entry_json = claude_panel_entry_json(&launcher, &physical)?;
                if want_json {
                    let report = panel_action_json(app, &alias, &physical, "install", false);
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
                if recorded_row.is_some() && found.matches_expected(&launcher, &physical) {
                    if want_json {
                        let report = panel_action_json(app, &alias, &physical, "install", false);
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
                return Err(Error::new(
                    "an unmanaged local oms-panel entry exists; remove it first",
                ));
            }

            let add_out = run_bounded_command(
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
            )?;
            if add_out.code != Some(0) {
                return Err(vendor_failure("add-json", &add_out));
            }

            let rows = panels.entry(alias.clone()).or_default();
            if let Some(slot) = rows.iter_mut().find(|r| r.app == app.as_str()) {
                slot.path = physical.clone();
            } else {
                rows.push(PanelRecord {
                    app: app.as_str().to_owned(),
                    scope: app.expected_scope().to_owned(),
                    name: PANEL_ENTRY_NAME.to_owned(),
                    path: physical.clone(),
                });
            }

            if let Err(record_err) =
                write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)
            {
                let rollback = run_bounded_command(
                    &claude_bin,
                    &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                    &physical,
                    stop,
                );
                return match rollback {
                    Ok(rb)
                        if rb.code == Some(0) || is_mcp_entry_missing(&rb.stdout, &rb.stderr) =>
                    {
                        Err(Error::new(format!(
                            "{}; rolled back local {PANEL_ENTRY_NAME} entry",
                            record_err.message
                        )))
                    }
                    Ok(rb) => Err(Error {
                        code: 1,
                        message: format!(
                            "{}; rollback via `claude mcp remove --scope local {PANEL_ENTRY_NAME}` also failed ({})",
                            record_err.message,
                            vendor_failure("remove", &rb).message
                        ),
                        publication_uncertain: true,
                    }),
                    Err(rb_err) => Err(Error {
                        code: 1,
                        message: format!(
                            "{}; rollback via `claude mcp remove --scope local {PANEL_ENTRY_NAME}` also failed ({})",
                            record_err.message, rb_err.message
                        ),
                        publication_uncertain: true,
                    }),
                };
            }

            if want_json {
                let report = panel_action_json(app, &alias, &physical, "install", true);
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
    }
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
    if let Ok(stat) = rustix::fs::statat(&dir_fd, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
        validate_map_stat(&stat)?;
    }
    if let Ok(stat) = rustix::fs::statat(
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
                let report = panel_action_json(app, &alias, &report_path, "uninstall", false);
                return Ok((0, json(&report)?, vec![]));
            }
            return Ok((
                0,
                format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
                vec![],
            ));
        };
        match app {
            PanelApp::Claude => {
                let claude_bin = resolve_claude_program(cwd, true)?;
                if want_json {
                    let report = panel_action_json(app, &alias, &row.path, "uninstall", false);
                    return Ok((0, json(&report)?, vec![]));
                }
                let text = format_vendor_command(
                    &row.path,
                    &claude_bin,
                    &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                )?;
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
    let Some(row) = recorded else {
        let report_path = std::fs::canonicalize(configured).unwrap_or_else(|_| configured.clone());
        if want_json {
            let report = panel_action_json(app, &alias, &report_path, "uninstall", false);
            return Ok((0, json(&report)?, vec![]));
        }
        return Ok((
            0,
            format!("no recorded {} panel for {alias}\n", app.as_str()).into_bytes(),
            vec![],
        ));
    };

    require(
        row.scope == app.expected_scope() && row.name == PANEL_ENTRY_NAME,
        "Invalid OMS project panel map: unexpected scope or entry name",
    )?;
    require(
        std::fs::metadata(&row.path).is_ok_and(|m| m.is_dir()),
        &format!(
            "recorded checkout path does not exist: {}",
            row.path.display()
        ),
    )?;

    let already_gone = match app {
        PanelApp::Claude => {
            let claude_bin = resolve_claude_program(cwd, false)?;
            let out = run_bounded_command(
                &claude_bin,
                &["mcp", "remove", "--scope", "local", PANEL_ENTRY_NAME],
                &row.path,
                stop,
            )?;
            if out.code == Some(0) {
                false
            } else if is_mcp_entry_missing(&out.stdout, &out.stderr) {
                true
            } else {
                return Err(vendor_failure("remove", &out));
            }
        }
    };

    if let Some(rows) = panels.get_mut(&alias) {
        rows.retain(|r| r.app != app.as_str());
        if rows.is_empty() {
            panels.remove(&alias);
        }
    }
    write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;

    if want_json {
        let report = panel_action_json(app, &alias, &row.path, "uninstall", true);
        return Ok((0, json(&report)?, vec![]));
    }
    let msg = if already_gone {
        format!(
            "{} panel {PANEL_ENTRY_NAME} for {alias} was already gone; dropped ownership record ({})\n",
            app.as_str(),
            row.path.display()
        )
    } else {
        format!(
            "uninstalled {} panel {PANEL_ENTRY_NAME} for {alias}: {}\n",
            app.as_str(),
            row.path.display()
        )
    };
    Ok((0, msg.into_bytes(), vec![]))
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeScope, PanelApp, claude_panel_entry_json, is_mcp_entry_missing, parse_claude_mcp_get,
        parse_panels_json,
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

        assert!(is_mcp_entry_missing(
            "",
            "No local MCP server found with name: oms-panel"
        ));
    }

    #[test]
    fn panel_app_and_sidecar_validation_stay_strict() {
        assert_eq!(
            oms_scrub::scan(include_bytes!("project_panels.rs"), &[]),
            Ok(Vec::new())
        );
        assert_eq!(PanelApp::parse("claude").unwrap(), PanelApp::Claude);
        let err = PanelApp::parse("codex").unwrap_err();
        assert!(err.message.contains("supported: claude"), "{}", err.message);

        let ok = r#"{"alpha":[{"app":"claude","scope":"local","name":"oms-panel","path":"/srv/alpha"}]}"#;
        assert_eq!(parse_panels_json(ok).unwrap().len(), 1);

        for bad in [
            r#"{"alpha":[]}"#,
            r#"{"alpha":[{"app":"codex","scope":"local","name":"oms-panel","path":"/srv/alpha"}]}"#,
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
