//! Retained pathname evidence for streaming reads and same-inode append.
use crate::{Error, Result};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}
fn changed() -> Error {
    Error::usage("pathname identity changed or cannot be revalidated; result is incomplete")
}
fn io(error: Errno) -> Error {
    Error::usage(format!("unsafe or unreadable path component: {error}"))
}
fn identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}
fn directory(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Directory
}
pub(super) fn regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile && stat.st_nlink == 1
}
pub(super) fn unchanged(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
        && right.is_file()
        && right.nlink() == 1
}
#[allow(clippy::unnecessary_cast)]
fn stat_matches(stat: &Stat, meta: &Metadata) -> bool {
    regular(stat)
        && stat.st_dev as u64 == meta.dev()
        && stat.st_ino as u64 == meta.ino()
        && stat.st_size as u64 == meta.len()
        && stat.st_mode as u32 == meta.mode()
        && stat.st_uid == meta.uid()
        && stat.st_gid == meta.gid()
        && stat.st_mtime == meta.mtime()
        && stat.st_mtime_nsec as i64 == meta.mtime_nsec()
        && stat.st_ctime == meta.ctime()
        && stat.st_ctime_nsec as i64 == meta.ctime_nsec()
}
pub(super) fn sync_directory(fd: &OwnedFd) -> Result<()> {
    match fs::fsync(fd) {
        Ok(()) | Err(Errno::BADF | Errno::INVAL | Errno::NOTSUP) => Ok(()),
        Err(e) => Err(Error::io(format!("directory durability failed: {e}"))),
    }
}
struct Directory {
    fd: OwnedFd,
    parent: Option<usize>,
    name: OsString,
    stat: Stat,
}
struct AliasEntry {
    path: PathBuf,
    meta: Metadata,
}
/// The caller root may have a stable logical spelling; protected descendants never follow links.
pub(super) struct Anchor {
    dirs: Vec<Directory>,
    aliases: Vec<AliasEntry>,
    logical_root: PathBuf,
    physical_root: PathBuf,
    parent: usize,
    name: OsString,
}
fn selected_root(path: &Path, caller: &Path) -> Result<PathBuf> {
    let physical = caller.canonicalize().map_err(|_| changed())?;
    if path.starts_with(caller) {
        return Ok(caller.to_owned());
    }
    if path.starts_with(&physical) {
        return Ok(physical);
    }
    // Accept an alternate spelling of the caller root (or its containing root),
    // before entering protected descendants. Prefer the earliest proved prefix.
    let mut prefix = PathBuf::new();
    let components = path.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        prefix.push(component.as_os_str());
        if let Ok(resolved) = prefix.canonicalize()
            && resolved != Path::new("/")
            && physical.starts_with(&resolved)
            && prefix.as_os_str() != resolved.as_os_str()
        {
            return Ok(prefix);
        }
    }
    #[cfg(target_os = "macos")]
    for (logical, physical) in [("/tmp", "/private/tmp"), ("/var", "/private/var")] {
        if path.starts_with(logical) {
            let resolved = Path::new(logical).canonicalize().map_err(|_| changed())?;
            if resolved != Path::new(physical) {
                return Err(changed());
            }
            return Ok(PathBuf::from(logical));
        }
    }
    Ok(PathBuf::from("/"))
}
impl Anchor {
    /// None is returned only after the retained prefix and the missing name are checked again.
    pub(super) fn parent(path: &Path, caller: &Path, create: bool) -> Result<Option<Self>> {
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(Error::usage("record path must be absolute without NUL"));
        }
        let spelling = path.as_os_str().as_encoded_bytes();
        if spelling.ends_with(b"/") || spelling.ends_with(b"/.") {
            return Err(Error::usage("record path ends with directory syntax"));
        }
        if !matches!(path.components().next_back(), Some(Component::Normal(_))) {
            return Err(Error::usage("record path requires a regular file name"));
        }
        let logical_root = selected_root(path, caller)?;
        let physical_root = logical_root.canonicalize().map_err(|_| changed())?;
        let mut aliases = vec![];
        if logical_root.as_os_str() != physical_root.as_os_str() {
            let mut prefix = PathBuf::new();
            for component in logical_root.components() {
                prefix.push(component.as_os_str());
                let meta = std::fs::symlink_metadata(&prefix).map_err(|_| changed())?;
                aliases.push(AliasEntry {
                    path: prefix.clone(),
                    meta,
                });
            }
        }
        let fd = fs::open("/", directory_flags(), Mode::empty()).map_err(io)?;
        let stat = fs::fstat(&fd).map_err(io)?;
        let mut result = Self {
            dirs: vec![Directory {
                fd,
                parent: None,
                name: OsString::from("/"),
                stat,
            }],
            aliases,
            logical_root,
            physical_root,
            parent: 0,
            name: OsString::new(),
        };
        let physical_parts = result
            .physical_root
            .components()
            .filter_map(|c| {
                if let Component::Normal(name) = c {
                    Some(name.to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for name in physical_parts {
            if !result.descend(&name, false)? {
                return Err(changed());
            }
        }
        result.verify()?;
        let relative = path
            .strip_prefix(&result.logical_root)
            .map_err(|_| changed())?;
        let components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(name)) = components.last() else {
            return Err(Error::usage("record path requires a regular file name"));
        };
        result.name = name.to_os_string();
        for component in &components[..components.len() - 1] {
            match component {
                Component::Normal(name) => {
                    if !result.descend(name, create)? {
                        return Ok(None);
                    }
                }
                // Open and retain the actual parent. Never cancel an unproved prefix lexically.
                Component::ParentDir => {
                    if !result.descend(OsStr::new(".."), false)? {
                        return Err(changed());
                    }
                }
                Component::CurDir => (),
                _ => return Err(Error::usage("invalid record path component")),
            }
        }
        result.verify()?;
        Ok(Some(result))
    }
    pub(super) fn fd(&self) -> &OwnedFd {
        &self.dirs[self.parent].fd
    }
    pub(super) fn name(&self) -> &OsStr {
        &self.name
    }
    fn descend(&mut self, name: &OsStr, create: bool) -> Result<bool> {
        let observed = match fs::statat(self.fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) if create => {
                self.verify()?;
                match fs::mkdirat(self.fd(), name, Mode::from_raw_mode(0o700)) {
                    Ok(()) => sync_directory(self.fd())?,
                    Err(Errno::EXIST) => (),
                    Err(e) => return Err(io(e)),
                }
                fs::statat(self.fd(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| changed())?
            }
            Err(Errno::NOENT) => {
                self.prove_missing(name)?;
                return Ok(false);
            }
            Err(e) => return Err(io(e)),
        };
        if !directory(&observed) {
            return Err(Error::usage(
                "protected path component must be a non-symlink directory",
            ));
        }
        let fd =
            fs::openat(self.fd(), name, directory_flags(), Mode::empty()).map_err(|_| changed())?;
        let held = fs::fstat(&fd).map_err(io)?;
        if !directory(&held) || !identity(&observed, &held) {
            return Err(changed());
        }
        self.dirs.push(Directory {
            fd,
            parent: Some(self.parent),
            name: name.to_owned(),
            stat: held,
        });
        self.parent = self.dirs.len() - 1;
        Ok(true)
    }
    pub(super) fn verify(&self) -> Result<()> {
        for entry in &self.aliases {
            let now = std::fs::symlink_metadata(&entry.path).map_err(|_| changed())?;
            if now.dev() != entry.meta.dev()
                || now.ino() != entry.meta.ino()
                || now.file_type() != entry.meta.file_type()
                || (now.file_type().is_symlink()
                    && (now.ctime() != entry.meta.ctime()
                        || now.ctime_nsec() != entry.meta.ctime_nsec()))
            {
                return Err(changed());
            }
        }
        if self.logical_root.canonicalize().map_err(|_| changed())? != self.physical_root {
            return Err(changed());
        }
        for node in &self.dirs {
            let named = if let Some(parent) = node.parent {
                fs::statat(&self.dirs[parent].fd, &node.name, AtFlags::SYMLINK_NOFOLLOW)
            } else {
                fs::stat("/")
            }
            .map_err(|_| changed())?;
            let held = fs::fstat(&node.fd).map_err(|_| changed())?;
            if !directory(&named)
                || !directory(&held)
                || !identity(&named, &node.stat)
                || !identity(&held, &node.stat)
            {
                return Err(changed());
            }
        }
        Ok(())
    }
    fn prove_missing(&self, name: &OsStr) -> Result<()> {
        self.verify()?;
        match fs::statat(self.fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => (),
            _ => return Err(changed()),
        }
        self.verify()
    }
    pub(super) fn leaf(&self) -> Result<Option<Stat>> {
        self.verify()?;
        match fs::statat(self.fd(), self.name(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(stat)),
            Err(Errno::NOENT) => {
                self.prove_missing(self.name())?;
                Ok(None)
            }
            Err(e) => Err(io(e)),
        }
    }
    pub(super) fn verify_file(&self, file: &File, expected: &Metadata) -> Result<()> {
        self.verify()?;
        let held = file.metadata().map_err(|_| changed())?;
        let named =
            fs::statat(self.fd(), self.name(), AtFlags::SYMLINK_NOFOLLOW).map_err(|_| changed())?;
        if !unchanged(expected, &held) || !stat_matches(&named, expected) {
            return Err(changed());
        }
        self.verify()
    }
    pub(super) fn physical_parent(&self, path: &Path) -> Result<PathBuf> {
        self.verify()?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::usage("missing record parent"))?
            .canonicalize()
            .map_err(|_| changed())?;
        let named = fs::stat(&parent).map_err(|_| changed())?;
        let held = fs::fstat(self.fd()).map_err(|_| changed())?;
        if !identity(&named, &held) || !directory(&named) {
            return Err(changed());
        }
        self.verify()?;
        Ok(parent)
    }
}
