use crate::{Error, Output, Result, cli::Options, json, parent_only, require};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DEFAULT_MAP_REL: &str = "oh-my-setting/desktop-projects.json";
const AREAS_FILE_NAME: &str = "desktop-project-areas.json";
const INVALID_MAP_MSG: &str = "Invalid OMS project map. Configure a nonempty object of project aliases and absolute checkout paths on the host.";
const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 16384;
const MAX_AREA_BYTES: usize = 24;

#[path = "project_panels.rs"]
mod project_panels;
pub(crate) use project_panels::PanelApp;

#[derive(Clone, Copy)]
enum TargetKind {
    ProjectMap,
    AreaSidecar,
    PanelSidecar,
}

impl TargetKind {
    const fn label(self) -> &'static str {
        match self {
            Self::ProjectMap => "desktop project map",
            Self::AreaSidecar => "desktop project area map",
            Self::PanelSidecar => "desktop project panel map",
        }
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn is_hangul(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{A960}'..='\u{A97F}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{D7B0}'..='\u{D7FF}'
    )
}

fn normalize_area(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c| matches!(c, ' ' | '\t'));
    ((1..=MAX_AREA_BYTES).contains(&trimmed.len())
        && trimmed.chars().all(|c| {
            c.is_ascii_alphabetic()
                || c.is_ascii_digit()
                || matches!(c, ' ' | '-' | '_' | '.')
                || is_hangul(c)
        }))
    .then(|| trimmed.to_owned())
}

enum AreaChange {
    Keep,
    Clear,
    Set(String),
}

fn parse_area_option(options: &Options) -> Result<AreaChange> {
    match options.get("area") {
        None => Ok(AreaChange::Keep),
        Some("") => Ok(AreaChange::Clear),
        Some(raw) => normalize_area(raw).map(AreaChange::Set).ok_or_else(|| {
            Error::new(format!(
                "invalid project area '{raw}': expected 1 to 24 bytes after trimming spaces using letters, digits, space, '-', '_', '.' or Hangul"
            ))
        }),
    }
}

pub(crate) fn valid_alias(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub(crate) fn run(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    if options.has("install-panel") {
        return project_panels::install_panel(options, cwd, stop);
    }
    if options.has("uninstall-panel") {
        return project_panels::uninstall_panel(options, cwd, stop);
    }
    if options.has("register-project") {
        return register_project(options, cwd, stop);
    }
    if options.has("unregister-project") {
        return unregister_project(options, cwd, stop);
    }
    list_projects(options)
}

fn suggest_alias(requested: &str, existing: &BTreeMap<String, PathBuf>) -> String {
    for index in 2..=256usize {
        let suffix = format!("-{index}");
        let prefix_len = requested.len().min(80 - suffix.len());
        let candidate = format!("{}{suffix}", &requested[..prefix_len]);
        if valid_alias(&candidate) && !existing.contains_key(&candidate) {
            return candidate;
        }
    }
    "project-2".to_owned()
}

fn check_alias(value: &str) -> Result<()> {
    require(
        valid_alias(value),
        &format!(
            "invalid project alias '{value}': expected 1 to 80 ASCII bytes starting with [A-Za-z0-9] followed by [A-Za-z0-9._-]"
        ),
    )
}

fn validate_checkout(raw_path: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    let candidate = match raw_path {
        Some(raw) => {
            require(!raw.is_empty(), "--register-project requires a path")?;
            let path = Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
        }
        None => cwd.to_path_buf(),
    };
    let physical = std::fs::canonicalize(&candidate).map_err(|e| {
        Error::new(format!(
            "cannot resolve project path {}: {e}",
            candidate.display()
        ))
    })?;
    require(
        physical.is_absolute() && physical.to_str().is_some(),
        "project path must be a valid UTF-8 absolute path",
    )?;
    let meta = std::fs::symlink_metadata(&physical).map_err(|e| {
        Error::new(format!(
            "cannot inspect project path {}: {e}",
            physical.display()
        ))
    })?;
    require(
        meta.is_dir(),
        &format!("project path is not a directory: {}", physical.display()),
    )?;
    require(
        is_checkout(&physical)?,
        &format!(
            "project path must be a Git checkout or an adopted OMS project: {}",
            physical.display()
        ),
    )?;
    Ok(physical)
}

fn is_checkout(dir: &Path) -> Result<bool> {
    for marker in [".oms", ".git"] {
        match dir.join(marker).symlink_metadata() {
            Ok(m) if marker == ".oms" && m.is_dir() => return Ok(true),
            Ok(m) if marker == ".git" && (m.is_dir() || m.is_file()) => {
                if oms_safe_fs::has_git_marker(dir) {
                    return Ok(true);
                }
                if m.is_dir() {
                    match dir.join(".git/HEAD").symlink_metadata() {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        _ => {
                            return Err(Error::new(format!(
                                "unusable .git directory in {}",
                                dir.display()
                            )));
                        }
                    }
                }
            }
            Ok(_) => {
                return Err(Error::new(format!(
                    "invalid {marker} entry in {}",
                    dir.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => {
                return Err(Error::new(format!(
                    "cannot inspect {marker} in {}: {e}",
                    dir.display()
                )));
            }
        }
    }
    Ok(false)
}

enum MapSource {
    Inline(OsString),
    File { path: PathBuf, explicit: bool },
    Unconfigured,
}

fn resolve_map_source() -> Result<MapSource> {
    let map_file = std::env::var_os("OMS_PANEL_PROJECTS_FILE");
    let inline = std::env::var_os("OMS_PANEL_PROJECTS");
    require(
        !(map_file.is_some() && inline.is_some()),
        "OMS_PANEL_PROJECTS and OMS_PANEL_PROJECTS_FILE cannot both be set",
    )?;
    // The desktop panel ignores the map when pinned to one checkout, so neither change nor list it.
    require(
        std::env::var_os("OMS_PANEL_REPO").is_none(),
        "OMS_PANEL_REPO pins this panel to one checkout and the desktop project map is not used; unset it",
    )?;
    if let Some(raw) = inline {
        return Ok(MapSource::Inline(raw));
    }
    let (path, explicit) = if let Some(path) = map_file {
        (PathBuf::from(path), true)
    } else if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        (PathBuf::from(config).join(DEFAULT_MAP_REL), false)
    } else if let Some(home) = std::env::var_os("HOME") {
        (
            PathBuf::from(home).join(".config").join(DEFAULT_MAP_REL),
            false,
        )
    } else {
        return Ok(MapSource::Unconfigured);
    };
    validate_map_path(&path)?;
    Ok(MapSource::File { path, explicit })
}

fn resolve_writer_path() -> Result<(PathBuf, bool)> {
    match resolve_map_source()? {
        MapSource::Inline(_) => Err(Error::new(
            "OMS_PANEL_PROJECTS sets an inline project map; unset it to change a project map file",
        )),
        MapSource::File { path, explicit } => Ok((path, explicit)),
        MapSource::Unconfigured => Err(Error::new(
            "cannot determine desktop project map path: set XDG_CONFIG_HOME, HOME or OMS_PANEL_PROJECTS_FILE",
        )),
    }
}

fn validate_map_path(path: &Path) -> Result<()> {
    let raw = path.as_os_str().as_bytes();
    require(
        path.is_absolute()
            && !raw.contains(&0)
            && !raw.ends_with(b"/")
            && path.file_name().is_some()
            && path.file_name() != Some(OsStr::new(AREAS_FILE_NAME))
            && path.file_name() != Some(OsStr::new(project_panels::PANELS_FILE_NAME))
            && path.parent().is_some()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "invalid desktop project map path: expected a normalized absolute file path",
    )
}

fn physical_parent_and_leaf(path: &Path, explicit: bool) -> Result<(PathBuf, OsString)> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("missing desktop project map directory"))?;
    if !explicit
        && let Ok(meta) = std::fs::symlink_metadata(parent)
        && meta.file_type().is_symlink()
    {
        return Err(Error::new(
            "refusing symlinked desktop project map directory",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| Error::new("missing desktop project map filename"))?
        .to_owned();
    let mut ancestor = parent;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| Error::new("invalid desktop project map directory"))?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error::new("invalid desktop project map ancestor"))?;
            }
            Err(e) => {
                return Err(Error::new(format!(
                    "cannot inspect desktop project map directory: {e}"
                )));
            }
        }
    }
    let mut resolved = std::fs::canonicalize(ancestor)
        .map_err(|e| Error::new(format!("cannot resolve desktop project map directory: {e}")))?;
    for part in missing.iter().rev() {
        resolved.push(part);
    }
    Ok((resolved, name))
}

fn lock_map_file(target: &Path, cwd: &Path, stop: &AtomicBool) -> Result<oms_lock::Guard> {
    let get = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
    let fallback_home = target
        .parent()
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let settings = oms_lock::Settings {
        mode: oms_lock::Mode::LegacyAuto,
        timeout: std::time::Duration::from_secs(oms_lock::legacy_seconds(
            get("OMS_LOCK_TIMEOUT").as_deref(),
            300,
        )),
        poll_max: std::time::Duration::from_secs(oms_lock::legacy_seconds(
            get("OMS_POLL_MAX_SECONDS").as_deref(),
            10,
        )),
        lock_dir: get("OMS_LOCK_DIR").map(PathBuf::from),
        home: get("HOME").map(PathBuf::from).unwrap_or(fallback_home),
        temp_dir: get("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp")),
        cwd: if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            std::env::current_dir().map_err(Error::new)?
        },
        search_path: get("PATH").unwrap_or_default(),
        force_mkdir: get("OMS_LOCK_FORCE_MKDIR").as_deref() == Some(OsStr::new("1")),
    };
    let lock = oms_lock::Lock::for_file(target, settings)
        .map_err(|e| Error::new(format!("cannot prepare desktop project map lock: {e}")))?;
    lock.acquire_with_cancel(|| stop.load(Ordering::Acquire))
        .map_err(|e| Error {
            code: e.exit_code(),
            message: format!("desktop project map lock unavailable: {e}"),
            publication_uncertain: false,
        })
}

struct StrictProjectMap(BTreeMap<String, PathBuf>);

impl<'de> serde::Deserialize<'de> for StrictProjectMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct MapVisitor;
        impl<'de> serde::de::Visitor<'de> for MapVisitor {
            type Value = StrictProjectMap;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object of project aliases and absolute checkout paths")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut access: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, String>()? {
                    if map.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate project alias '{key}'"
                        )));
                    }
                    map.insert(key, PathBuf::from(value));
                }
                Ok(StrictProjectMap(map))
            }
        }
        deserializer.deserialize_map(MapVisitor)
    }
}

fn parse_projects_json(raw: &str) -> Result<BTreeMap<String, PathBuf>> {
    require(
        raw.len() <= MAX_BYTES,
        "Invalid OMS project map: exceeds 16 KiB",
    )?;
    let StrictProjectMap(map) =
        serde_json::from_str(raw).map_err(|_| Error::new(INVALID_MAP_MSG))?;
    require(
        !map.is_empty()
            && map.len() <= MAX_ENTRIES
            && map.iter().all(|(id, p)| {
                valid_alias(id) && p.is_absolute() && !p.as_os_str().as_bytes().contains(&0)
            }),
        INVALID_MAP_MSG,
    )?;
    Ok(map)
}

struct StrictAreaMap(BTreeMap<String, String>);

impl<'de> serde::Deserialize<'de> for StrictAreaMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct AreaVisitor;
        impl<'de> serde::de::Visitor<'de> for AreaVisitor {
            type Value = StrictAreaMap;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object of project aliases and area strings")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut access: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, String>()? {
                    if map.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate project alias '{key}'"
                        )));
                    }
                    map.insert(key, value);
                }
                Ok(StrictAreaMap(map))
            }
        }
        deserializer.deserialize_map(AreaVisitor)
    }
}

fn parse_areas_json(raw: &str) -> Result<BTreeMap<String, String>> {
    require(
        raw.len() <= MAX_BYTES,
        "Invalid OMS project area map: exceeds 16 KiB",
    )?;
    let StrictAreaMap(raw_map) = serde_json::from_str(raw).map_err(|_| {
        Error::new("Invalid OMS project area map: expected an object of project aliases and valid area strings")
    })?;
    require(
        raw_map.len() <= MAX_ENTRIES,
        "Invalid OMS project area map: exceeds 128 entries",
    )?;
    let mut areas = BTreeMap::new();
    for (id, raw_area) in raw_map {
        let Some(clean) = valid_alias(&id)
            .then(|| normalize_area(&raw_area))
            .flatten()
        else {
            return Err(Error::new(
                "Invalid OMS project area map: expected valid project aliases and 1 to 24 byte areas",
            ));
        };
        areas.insert(id, clean);
    }
    Ok(areas)
}

fn encode_areas_json(areas: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    require(
        !areas.is_empty() && areas.len() <= MAX_ENTRIES,
        "desktop project area map must contain 1 to 128 entries",
    )?;
    let mut bytes = serde_json::to_vec_pretty(areas).map_err(Error::new)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        bytes = serde_json::to_vec(areas).map_err(Error::new)?;
        bytes.push(b'\n');
    }
    require(
        bytes.len() <= MAX_BYTES,
        "desktop project area map exceeds 16 KiB",
    )?;
    Ok(bytes)
}

fn encode_projects_json(map: &BTreeMap<String, PathBuf>) -> Result<Vec<u8>> {
    require(
        !map.is_empty() && map.len() <= MAX_ENTRIES,
        "desktop project map must contain 1 to 128 entries",
    )?;
    let mut bytes = serde_json::to_vec_pretty(map).map_err(Error::new)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        bytes = serde_json::to_vec(map).map_err(Error::new)?;
        bytes.push(b'\n');
    }
    require(
        bytes.len() <= MAX_BYTES,
        "desktop project map exceeds 16 KiB",
    )?;
    Ok(bytes)
}

fn validate_map_stat(stat: &Stat) -> Result<()> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(Error::new("refusing symlinked desktop project map"));
    }
    require(
        kind == FileType::RegularFile,
        "Invalid OMS project map: not a regular file",
    )?;
    require(
        stat.st_nlink == 1,
        "Invalid OMS project map: file must not have hard links",
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        "Invalid OMS project map: file is not owned by the current user",
    )?;
    require(
        stat.st_mode & 0o022 == 0,
        "Invalid OMS project map: file must not be group or world writable",
    )?;
    Ok(())
}

fn validate_private_sidecar_stat(
    stat: &Stat,
    invalid_prefix: &str,
    symlink_label: &str,
) -> Result<()> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(Error::new(format!("refusing symlinked {symlink_label}")));
    }
    require(
        kind == FileType::RegularFile,
        &format!("Invalid {invalid_prefix}: not a regular file"),
    )?;
    require(
        stat.st_nlink == 1,
        &format!("Invalid {invalid_prefix}: file must not have hard links"),
    )?;
    require(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        &format!("Invalid {invalid_prefix}: file is not owned by the current user"),
    )?;
    require(
        stat.st_mode & 0o077 == 0,
        &format!("Invalid {invalid_prefix}: file must be private to the current user"),
    )?;
    Ok(())
}

fn validate_areas_stat(stat: &Stat) -> Result<()> {
    validate_private_sidecar_stat(stat, "OMS project area map", "desktop project area map")
}

fn same_stat(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_nlink == b.st_nlink
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}

fn open_parent_dir(parent: &Path, create: bool) -> Result<Option<OwnedFd>> {
    if !parent.exists() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::new(
                    "refusing symlinked desktop project map directory",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(|err| {
                        Error::new(format!(
                            "cannot create desktop project map directory {}: {err}",
                            parent.display()
                        ))
                    })?;
            }
            Err(e) => {
                return Err(Error::new(format!(
                    "cannot inspect desktop project map directory {}: {e}",
                    parent.display()
                )));
            }
        }
    }
    match fs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::LOOP) => Err(Error::new(
            "refusing symlinked desktop project map directory",
        )),
        Err(Errno::NOENT) if !create => Ok(None),
        Err(e) => Err(Error::new(format!(
            "cannot open desktop project map directory {}: {e}",
            parent.display()
        ))),
    }
}

fn read_map_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
) -> Result<Option<(BTreeMap<String, PathBuf>, Stat)>> {
    let named = match fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            validate_map_stat(&stat)?;
            stat
        }
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => {
            return Err(Error::new(format!(
                "cannot inspect desktop project map: {e}"
            )));
        }
    };
    let fd = match fs::openat(
        dir_fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new("refusing symlinked desktop project map")),
        Err(e) => return Err(Error::new(format!("cannot open desktop project map: {e}"))),
    };
    let opened = fs::fstat(&fd)
        .map_err(|e| Error::new(format!("cannot inspect desktop project map: {e}")))?;
    validate_map_stat(&opened)?;
    require(
        same_stat(&named, &opened),
        "desktop project map changed while opening",
    )?;
    let mut file = std::fs::File::from(fd);
    let mut raw = String::new();
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_string(&mut raw)
        .map_err(|_| Error::new("Invalid OMS project map: file is not UTF-8"))?;
    let after = fs::fstat(&file)
        .map_err(|e| Error::new(format!("cannot verify desktop project map: {e}")))?;
    require(
        same_stat(&opened, &after),
        "desktop project map changed while reading",
    )?;
    let map = parse_projects_json(&raw)?;
    Ok(Some((map, after)))
}

fn read_private_sidecar_at(
    dir_fd: &OwnedFd,
    name: &OsStr,
    invalid_prefix: &str,
    label: &str,
) -> Result<Option<(String, Stat)>> {
    let named = match fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            validate_private_sidecar_stat(&stat, invalid_prefix, label)?;
            stat
        }
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => {
            return Err(Error::new(format!("cannot inspect {label}: {e}")));
        }
    };
    let fd = match fs::openat(
        dir_fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(Error::new(format!("refusing symlinked {label}"))),
        Err(e) => {
            return Err(Error::new(format!("cannot open {label}: {e}")));
        }
    };
    let opened = fs::fstat(&fd).map_err(|e| Error::new(format!("cannot inspect {label}: {e}")))?;
    validate_private_sidecar_stat(&opened, invalid_prefix, label)?;
    require(
        same_stat(&named, &opened),
        &format!("{label} changed while opening"),
    )?;
    let mut file = std::fs::File::from(fd);
    let mut raw = String::new();
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_string(&mut raw)
        .map_err(|_| Error::new(format!("Invalid {invalid_prefix}: file is not UTF-8")))?;
    let after = fs::fstat(&file).map_err(|e| Error::new(format!("cannot verify {label}: {e}")))?;
    require(
        same_stat(&opened, &after),
        &format!("{label} changed while reading"),
    )?;
    Ok(Some((raw, after)))
}

fn read_areas_at(dir_fd: &OwnedFd) -> Result<Option<(BTreeMap<String, String>, Stat)>> {
    let Some((raw, after)) = read_private_sidecar_at(
        dir_fd,
        OsStr::new(AREAS_FILE_NAME),
        "OMS project area map",
        "desktop project area map",
    )?
    else {
        return Ok(None);
    };
    let areas = parse_areas_json(&raw)?;
    Ok(Some((areas, after)))
}

struct TempMapFile<'a> {
    dir_fd: &'a OwnedFd,
    name: OsString,
    file: std::fs::File,
    initial: Stat,
    published: bool,
}

impl<'a> TempMapFile<'a> {
    fn create(dir_fd: &'a OwnedFd, target_name: &OsStr) -> Result<Self> {
        let mut name = OsString::from(".");
        name.push(target_name);
        name.push(format!(
            ".tmp.{}.{}.{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let fd = fs::openat(
            dir_fd,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| Error::new(format!("cannot stage desktop project map: {e}")))?;
        fs::fchmod(&fd, Mode::from_raw_mode(0o600))
            .map_err(|e| Error::new(format!("cannot set 0600 on desktop project map: {e}")))?;
        let initial = fs::fstat(&fd)
            .map_err(|e| Error::new(format!("cannot verify staged desktop project map: {e}")))?;
        Ok(Self {
            dir_fd,
            name,
            file: std::fs::File::from(fd),
            initial,
            published: false,
        })
    }

    fn owns_entry(&self) -> bool {
        fs::statat(self.dir_fd, &self.name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
            FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_nlink == 1
                && stat.st_dev == self.initial.st_dev
                && stat.st_ino == self.initial.st_ino
        })
    }
}

impl Drop for TempMapFile<'_> {
    fn drop(&mut self) {
        if !self.published && self.owns_entry() {
            let _ = fs::unlinkat(self.dir_fd, &self.name, AtFlags::empty());
        }
    }
}

fn sync_dir(dir_fd: &OwnedFd) -> Result<()> {
    match fs::fsync(dir_fd) {
        Ok(()) | Err(Errno::BADF | Errno::INVAL | Errno::NOTSUP) => Ok(()),
        Err(e) => Err(Error {
            code: 1,
            message: format!("cannot persist desktop project map directory: {e}"),
            publication_uncertain: true,
        }),
    }
}

fn validate_target_stat(stat: &Stat, kind: TargetKind) -> Result<()> {
    match kind {
        TargetKind::ProjectMap => validate_map_stat(stat),
        TargetKind::AreaSidecar => validate_areas_stat(stat),
        TargetKind::PanelSidecar => project_panels::validate_panels_stat(stat),
    }
}

fn check_destination_unchanged(
    dir_fd: &OwnedFd,
    name: &OsStr,
    before: Option<&Stat>,
    kind: TargetKind,
) -> Result<()> {
    let label = kind.label();
    match (before, fs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW)) {
        (None, Err(Errno::NOENT)) => Ok(()),
        (None, Ok(stat)) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            Err(Error::new(format!("refusing symlinked {label}")))
        }
        (None, Ok(_)) => Err(Error::new(format!(
            "{label} was created concurrently; retry"
        ))),
        (Some(prev), Ok(curr)) => {
            validate_target_stat(&curr, kind)?;
            require(
                same_stat(prev, &curr),
                &format!("{label} changed concurrently; retry"),
            )
        }
        (Some(_), Err(Errno::NOENT)) => Err(Error::new(format!(
            "{label} disappeared concurrently; retry"
        ))),
        (_, Err(e)) => Err(Error::new(format!(
            "cannot verify {label} destination: {e}"
        ))),
    }
}

fn write_atomic(
    dir_fd: &OwnedFd,
    name: &OsStr,
    before: Option<&Stat>,
    bytes: &[u8],
    kind: TargetKind,
) -> Result<()> {
    let mut temp = TempMapFile::create(dir_fd, name)?;
    temp.file
        .write_all(bytes)
        .map_err(|e| Error::new(format!("cannot write desktop project map: {e}")))?;
    fs::fchmod(&temp.file, Mode::from_raw_mode(0o600))
        .map_err(|e| Error::new(format!("cannot set 0600 on desktop project map: {e}")))?;
    fs::fsync(&temp.file)
        .map_err(|e| Error::new(format!("cannot sync staged desktop project map: {e}")))?;
    let staged = fs::fstat(&temp.file)
        .map_err(|e| Error::new(format!("cannot verify staged desktop project map: {e}")))?;
    validate_target_stat(&staged, kind)?;
    require(
        staged.st_mode & 0o7777 == 0o600,
        "staged desktop project map did not retain mode 0600",
    )?;
    require(
        temp.owns_entry(),
        "staged desktop project map changed before publication",
    )?;
    check_destination_unchanged(dir_fd, name, before, kind)?;
    fs::renameat(dir_fd, &temp.name, dir_fd, name).map_err(|e| Error {
        code: 1,
        message: format!("cannot publish {}: {e}", kind.label()),
        publication_uncertain: true,
    })?;
    temp.published = true;
    sync_dir(dir_fd)
}

fn write_or_unlink_areas(
    dir_fd: &OwnedFd,
    before: Option<&Stat>,
    areas: &BTreeMap<String, String>,
) -> Result<()> {
    let name = OsStr::new(AREAS_FILE_NAME);
    if areas.is_empty() {
        check_destination_unchanged(dir_fd, name, before, TargetKind::AreaSidecar)?;
        if before.is_some() {
            fs::unlinkat(dir_fd, name, AtFlags::empty()).map_err(|e| Error {
                code: 1,
                message: format!("cannot remove empty desktop project area map: {e}"),
                publication_uncertain: true,
            })?;
            sync_dir(dir_fd)?;
        }
        Ok(())
    } else {
        let bytes = encode_areas_json(areas)?;
        write_atomic(dir_fd, name, before, &bytes, TargetKind::AreaSidecar)
    }
}

fn register_project(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let (target, explicit) = resolve_writer_path()?;
    let area_change = parse_area_option(options)?;
    let physical = validate_checkout(options.get("register-project"), cwd)?;
    let alias = match options.get("alias") {
        Some(explicit_alias) => explicit_alias.to_owned(),
        None => physical
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| {
                Error::new("cannot derive project alias from checkout path; pass --alias NAME")
            })?
            .to_owned(),
    };
    check_alias(&alias)?;

    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
    // Before creating directories or locks, refuse an existing unsafe leaf if its parent exists.
    if let Some(pre_dir) = open_parent_dir(&parent, false)? {
        if let Ok(stat) = fs::statat(&pre_dir, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
            validate_map_stat(&stat)?;
        }
        if let Ok(stat) = fs::statat(
            &pre_dir,
            OsStr::new(AREAS_FILE_NAME),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            validate_areas_stat(&stat)?;
        }
    }

    let dir_fd = open_parent_dir(&parent, true)?
        .ok_or_else(|| Error::new("desktop project map directory unavailable"))?;
    let lock_target = parent.join(&leaf_name);
    let _guard = lock_map_file(&lock_target, cwd, stop)?;

    let existing = read_map_at(&dir_fd, &leaf_name)?;
    let (mut map, before_stat) = match existing {
        Some((map, stat)) => (map, Some(stat)),
        None => (BTreeMap::new(), None),
    };
    let (mut areas, before_areas_stat) = match read_areas_at(&dir_fd)? {
        Some((areas, stat)) => (areas, Some(stat)),
        None => (BTreeMap::new(), None),
    };

    if map.get(&alias) == Some(&physical) {
        let area_changed = match area_change {
            AreaChange::Keep => false,
            AreaChange::Clear => areas.remove(&alias).is_some(),
            AreaChange::Set(new_area) => {
                if areas.get(&alias) == Some(&new_area) {
                    false
                } else {
                    areas.insert(alias.clone(), new_area);
                    true
                }
            }
        };
        if !area_changed {
            return Ok((
                0,
                format!("already registered {alias}: {}\n", physical.display()).into_bytes(),
                vec![],
            ));
        }
        areas.retain(|k, _| map.contains_key(k));
        write_or_unlink_areas(&dir_fd, before_areas_stat.as_ref(), &areas)?;
        return Ok((
            0,
            format!("registered {alias}: {}\n", physical.display()).into_bytes(),
            vec![],
        ));
    }
    if let Some(other) = map.get(&alias) {
        let suggestion = suggest_alias(&alias, &map);
        return Err(Error::new(format!(
            "project alias '{alias}' already points to {}; choose another alias such as --alias {suggestion}",
            other.display()
        )));
    }
    require(
        map.len() < MAX_ENTRIES,
        "desktop project map is full (at most 128 entries)",
    )?;
    map.insert(alias.clone(), physical.clone());
    let bytes = encode_projects_json(&map)?;
    let area_changed = match area_change {
        AreaChange::Keep => false,
        AreaChange::Clear => areas.remove(&alias).is_some(),
        AreaChange::Set(new_area) => {
            areas.insert(alias.clone(), new_area);
            true
        }
    };
    write_atomic(
        &dir_fd,
        &leaf_name,
        before_stat.as_ref(),
        &bytes,
        TargetKind::ProjectMap,
    )?;
    if area_changed {
        areas.retain(|k, _| map.contains_key(k));
        write_or_unlink_areas(&dir_fd, before_areas_stat.as_ref(), &areas)?;
    }
    Ok((
        0,
        format!("registered {alias}: {}\n", physical.display()).into_bytes(),
        vec![],
    ))
}

fn unregister_project(options: &Options, cwd: &Path, stop: &AtomicBool) -> Result<Output> {
    parent_only()?;
    let (target, explicit) = resolve_writer_path()?;
    let alias = options.required("unregister-project")?;
    check_alias(&alias)?;

    let (parent, leaf_name) = physical_parent_and_leaf(&target, explicit)?;
    let Some(dir_fd) = open_parent_dir(&parent, false)? else {
        return Err(Error::new(format!(
            "project alias '{alias}' is not registered"
        )));
    };
    if let Ok(stat) = fs::statat(&dir_fd, &leaf_name, AtFlags::SYMLINK_NOFOLLOW) {
        validate_map_stat(&stat)?;
    }
    if let Ok(stat) = fs::statat(
        &dir_fd,
        OsStr::new(AREAS_FILE_NAME),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        validate_areas_stat(&stat)?;
    }
    if let Ok(stat) = fs::statat(
        &dir_fd,
        OsStr::new(project_panels::PANELS_FILE_NAME),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        project_panels::validate_panels_stat(&stat)?;
    }

    let lock_target = parent.join(&leaf_name);
    let _guard = lock_map_file(&lock_target, cwd, stop)?;

    let Some((mut map, before_stat)) = read_map_at(&dir_fd, &leaf_name)? else {
        return Err(Error::new(format!(
            "project alias '{alias}' is not registered"
        )));
    };
    let (mut areas, before_areas_stat) = match read_areas_at(&dir_fd)? {
        Some((areas, stat)) => (areas, Some(stat)),
        None => (BTreeMap::new(), None),
    };
    let (panels, before_panels_stat) = match project_panels::read_panels_at(&dir_fd)? {
        Some((panels, stat)) => (panels, Some(stat)),
        None => (BTreeMap::new(), None),
    };
    let Some(removed) = map.remove(&alias) else {
        return Err(Error::new(format!(
            "project alias '{alias}' is not registered"
        )));
    };
    if let Some(installed) = panels.get(&alias).filter(|rows| !rows.is_empty()) {
        let apps = installed
            .iter()
            .map(|r| r.app.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::new(format!(
            "project alias '{alias}' still has an installed panel ({apps}); run `oms panel --uninstall-panel {} --project {alias}` first",
            installed[0].app
        )));
    }

    // The reader rejects a missing explicitly named map, so keep its last entry instead of deleting it.
    require(
        !(map.is_empty() && explicit),
        "refusing to remove the last alias from the OMS_PANEL_PROJECTS_FILE map: the desktop panel needs at least one entry there; unset OMS_PANEL_PROJECTS_FILE or delete the file",
    )?;
    if map.is_empty() {
        check_destination_unchanged(
            &dir_fd,
            &leaf_name,
            Some(&before_stat),
            TargetKind::ProjectMap,
        )?;
        fs::unlinkat(&dir_fd, &leaf_name, AtFlags::empty()).map_err(|e| Error {
            code: 1,
            message: format!("cannot remove empty desktop project map: {e}"),
            publication_uncertain: true,
        })?;
        sync_dir(&dir_fd)?;
    } else {
        let bytes = encode_projects_json(&map)?;
        write_atomic(
            &dir_fd,
            &leaf_name,
            Some(&before_stat),
            &bytes,
            TargetKind::ProjectMap,
        )?;
    }
    let area_removed = areas.remove(&alias).is_some() || (map.is_empty() && !areas.is_empty());
    if area_removed {
        areas.retain(|k, _| map.contains_key(k));
        write_or_unlink_areas(&dir_fd, before_areas_stat.as_ref(), &areas)?;
    }
    if map.is_empty() && panels.is_empty() && before_panels_stat.is_some() {
        project_panels::write_or_unlink_panels(&dir_fd, before_panels_stat.as_ref(), &panels)?;
    }

    Ok((
        0,
        format!("unregistered {alias}: {}\n", removed.display()).into_bytes(),
        vec![],
    ))
}

fn list_projects(options: &Options) -> Result<Output> {
    parent_only().map_err(|_| Error::new("a delegated worker cannot list desktop projects"))?;
    let (file_path, map, areas) = match resolve_map_source()? {
        MapSource::Inline(raw) => {
            let text = raw.into_string().map_err(|_| {
                Error::new("Invalid OMS project map: OMS_PANEL_PROJECTS is not UTF-8")
            })?;
            (None, parse_projects_json(&text)?, BTreeMap::new())
        }
        MapSource::Unconfigured => (None, BTreeMap::new(), BTreeMap::new()),
        MapSource::File { path, explicit } => {
            let (parent, leaf_name) = physical_parent_and_leaf(&path, explicit)?;
            let resolved_file = parent.join(&leaf_name);
            let (map, areas) = match open_parent_dir(&parent, false)? {
                Some(dir_fd) => {
                    let areas = read_areas_at(&dir_fd)?.map(|(a, _)| a).unwrap_or_default();
                    match read_map_at(&dir_fd, &leaf_name)? {
                        Some((map, _)) => (map, areas),
                        None if explicit => return Err(Error::new(INVALID_MAP_MSG)),
                        None => (BTreeMap::new(), BTreeMap::new()),
                    }
                }
                None if explicit => return Err(Error::new(INVALID_MAP_MSG)),
                None => (BTreeMap::new(), BTreeMap::new()),
            };
            (Some(resolved_file), map, areas)
        }
    };

    if options.has("json") {
        let entries: Vec<serde_json::Value> = map
            .iter()
            .map(|(alias, path)| {
                let exists = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
                json!({
                    "alias": alias,
                    "area": areas.get(alias),
                    "path": path,
                    "exists": exists,
                })
            })
            .collect();
        let report = json!({
            "schema": 1,
            "kind": "oms-panel-projects",
            "file": file_path,
            "projects": entries,
        });
        return Ok((0, json(&report)?, vec![]));
    }

    if map.is_empty() {
        return Ok((0, b"no desktop projects registered\n".to_vec(), vec![]));
    }
    let mut out = String::new();
    for (alias, path) in &map {
        let area_col = match areas.get(alias) {
            Some(area) => format!("\t{area}"),
            None => String::new(),
        };
        if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            out.push_str(&format!("{alias}{area_col}\t{}\n", path.display()));
        } else {
            out.push_str(&format!(
                "{alias}{area_col}\t{}\t(path no longer exists)\n",
                path.display()
            ));
        }
    }
    Ok((0, out.into_bytes(), vec![]))
}
