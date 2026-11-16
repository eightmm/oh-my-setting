use super::*;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;

pub(super) fn target(root: &Root, repo: &Path, input: &Path) -> Result<PathBuf, StoreError> {
    if input.as_os_str().is_empty() || input.as_os_str().as_bytes().contains(&0) {
        return Err(StoreError::Invalid("invalid index path"));
    }
    let logical = crate::normalize(&std::env::current_dir()?.join(repo));
    let path = if input.is_absolute() {
        let normalized = crate::normalize(input);
        if let Ok(suffix) = normalized.strip_prefix(&logical) {
            root.physical_root().join(suffix)
        } else {
            normalized
        }
    } else {
        root.physical_root().join(input)
    };
    let path = crate::normalize(&path);
    if path == root.physical_root() || !path.starts_with(root.physical_root()) {
        return Err(StoreError::Invalid(
            "path must stay inside physical repository",
        ));
    }
    Ok(path)
}
pub(super) fn same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
pub(super) fn unchanged(a: &Stat, b: &Stat) -> bool {
    same(a, b)
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
        && a.st_nlink == b.st_nlink
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
}
pub(super) fn regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
        && stat.st_nlink == 1
        && stat.st_uid == rustix::process::getuid().as_raw()
}
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
pub(super) fn sync(fd: &OwnedFd) -> Result<(), StoreError> {
    match fs::fsync(fd) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::BADF | rustix::io::Errno::NOTSUP) => {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Open descriptors retain the original ancestry; revalidation rejects renames.
pub(super) struct Directory {
    pub fd: OwnedFd,
    identity: Stat,
    root_path: PathBuf,
    root_identity: Stat,
    ancestors: Vec<(OsString, Stat)>,
}
impl Directory {
    pub fn open(root: &Root, path: &Path, create: bool) -> Result<Self, StoreError> {
        let relative = path
            .strip_prefix(root.physical_root())
            .map_err(|_| StoreError::Invalid("external directory"))?;
        let mut fd = fs::open(root.physical_root(), dir_flags(), Mode::empty())?;
        let root_identity = fs::fstat(&fd)?;
        let mut ancestors = Vec::new();
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(StoreError::Invalid("unsafe directory component"));
            };
            let next = fs::openat(&fd, name, dir_flags(), Mode::empty());
            let next = match next {
                Err(rustix::io::Errno::NOENT) if create => {
                    match fs::mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => sync(&fd)?,
                        Err(rustix::io::Errno::EXIST) => (),
                        Err(error) => return Err(error.into()),
                    }
                    fs::openat(&fd, name, dir_flags(), Mode::empty())?
                }
                other => other?,
            };
            fd = next;
            ancestors.push((name.to_os_string(), fs::fstat(&fd)?));
        }
        let identity = fs::fstat(&fd)?;
        let result = Self {
            fd,
            identity,
            root_path: root.physical_root().to_path_buf(),
            root_identity,
            ancestors,
        };
        result.verify()?;
        Ok(result)
    }
    pub fn verify(&self) -> Result<(), StoreError> {
        let mut fd = fs::open(&self.root_path, dir_flags(), Mode::empty())?;
        if !same(&self.root_identity, &fs::fstat(&fd)?) {
            return Err(StoreError::Changed);
        }
        for (name, before) in &self.ancestors {
            fd = fs::openat(&fd, name, dir_flags(), Mode::empty())?;
            if !same(before, &fs::fstat(&fd)?) {
                return Err(StoreError::Changed);
            }
        }
        if !same(&self.identity, &fs::fstat(fd)?) {
            return Err(StoreError::Changed);
        }
        Ok(())
    }
    pub fn names(&self, max: usize) -> Result<Vec<OsString>, StoreError> {
        self.verify()?;
        let mut names = Vec::new();
        for entry in fs::Dir::read_from(&self.fd)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if names.len() == max {
                return Err(StoreError::Invalid("directory entry limit exceeded"));
            }
            names.push(OsStr::from_bytes(name).to_os_string());
        }
        self.verify()?;
        Ok(names)
    }
    pub fn stat(&self, name: &OsStr) -> Result<Stat, StoreError> {
        Ok(fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW)?)
    }
}
pub(super) fn missing(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Storage {
            error: ByteError::Io(rustix::io::Errno::NOENT) | ByteError::Missing,
            ..
        }
    )
}
pub(super) fn validate(root: &Root, target: &Path, create: bool) -> Result<(), StoreError> {
    let parent = Directory::open(
        root,
        target
            .parent()
            .ok_or(StoreError::Invalid("missing parent"))?,
        create,
    );
    let parent = match parent {
        Err(error) if !create && missing(&error) => return Ok(()),
        other => other?,
    };
    match parent.stat(
        target
            .file_name()
            .ok_or(StoreError::Invalid("missing leaf"))?,
    ) {
        Ok(stat) if regular(&stat) => (),
        Ok(_) => {
            return Err(StoreError::Invalid(
                "leaf must be an owned single-link regular file",
            ));
        }
        Err(error) if missing(&error) => (),
        Err(error) => return Err(error),
    }
    parent.verify()
}
