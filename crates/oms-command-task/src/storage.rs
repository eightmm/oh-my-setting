use crate::{
    Error, Result, checkpoint,
    services::{ReferenceOperation, ReferenceWrite},
};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Policy, Root};
use rustix::fs::{self, AtFlags, Mode, OFlags, RenameFlags};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::Read,
    os::{
        fd::OwnedFd,
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

pub(crate) const MAX_PACKET: usize = 32 * 1024 * 1024;
pub(crate) struct Store {
    root: Root,
    pub path: PathBuf,
    relative: PathBuf,
}
fn error(op: &str, e: impl std::fmt::Debug) -> Error {
    Error::new(1, format!("{op}: {e:?}"))
}
fn sync_directory(fd: &OwnedFd, op: &str) -> Result<()> {
    match fs::fsync(fd) {
        Ok(())
        | Err(rustix::io::Errno::BADF | rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => {
            Ok(())
        }
        Err(e) => Err(error(op, e)),
    }
}
pub(crate) fn normalize(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                let info = std::fs::symlink_metadata(&out)
                    .map_err(|e| Error::io("resolve parent traversal", e))?;
                if !info.is_dir() || info.file_type().is_symlink() {
                    return Err(Error::new(1, "unsafe task parent traversal"));
                }
                if !out.pop() {
                    return Err(Error::new(1, "invalid task path"));
                }
            }
            Component::Normal(v) => out.push(v),
            _ => return Err(Error::new(1, "unsupported task path")),
        }
    }
    if !out.is_absolute() || out.file_name().is_none() {
        return Err(Error::new(1, "invalid task path"));
    }
    Ok(out)
}
impl Store {
    pub fn new(repo: &Path, path: &Path) -> Result<Self> {
        let path = normalize(path)?;
        let base = if path.starts_with(repo) {
            repo.to_path_buf()
        } else {
            let mut parent = path
                .parent()
                .ok_or_else(|| Error::new(1, "task path has no parent"))?;
            while !parent.exists() {
                parent = parent
                    .parent()
                    .ok_or_else(|| Error::new(1, "task parent unavailable"))?;
            }
            parent.to_path_buf()
        };
        let root = Root::open(&base).map_err(|e| error("open task root", e))?;
        let relative = path
            .strip_prefix(&base)
            .map_err(|_| Error::new(1, "task outside storage root"))?
            .to_path_buf();
        let physical = root.physical_root().join(&relative);
        Ok(Self {
            root,
            path: physical,
            relative,
        })
    }
    pub fn read(&self) -> Result<Option<Vec<u8>>> {
        read(&self.root, &self.relative, MAX_PACKET)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self,
        repo: &Path,
        operation: &str,
        args: &[Vec<u8>],
        before: Option<&[u8]>,
        body: &[u8],
        source: &[u8],
        reference: &mut dyn ReferenceOperation,
        interrupted: &AtomicBool,
        signal: Option<&AtomicI32>,
    ) -> Result<()> {
        checkpoint(interrupted, signal)?;
        // Read with owner policy immediately before the safe-fs snapshot; the
        // transform compares exact bytes and existence before publication.
        if self.read()?.as_deref() != before {
            return Err(Error::new(1, "task packet changed before publication"));
        }
        reference.validate(&ReferenceWrite {
            repo,
            target: &self.path,
            operation,
            arguments: args,
            before: before.unwrap_or_default(),
            proposed: body,
            source_session: source,
        })?;
        self.root
            .atomic_mutate(
                &self.relative,
                MutationOptions {
                    read: ByteReadOptions {
                        missing_ok: true,
                        max_bytes: MAX_PACKET,
                    },
                    create_parent: true,
                    max_output: MAX_PACKET,
                    expected_exists: Some(before.is_some()),
                    ..MutationOptions::default()
                },
                |old| {
                    checkpoint(interrupted, signal)?;
                    if old != before.unwrap_or_default() {
                        return Err(Error::new(1, "task packet changed during publication"));
                    }
                    if let Ok(info) = std::fs::symlink_metadata(&self.path)
                        && info.uid() != rustix::process::getuid().as_raw()
                    {
                        return Err(Error::new(1, "task packet owner changed"));
                    }
                    Ok(body.to_vec())
                },
            )
            .map_err(|e| match e {
                oms_safe_fs::MutationError::Transform(e) => e,
                other => error("task publication (reconcile before retry)", other),
            })?;
        Ok(())
    }
    pub fn ensure_ignore(&self) {
        // Best effort, exactly like agent_memory_ensure_oms_ignore_for_path.
        let mut current = self.path.parent();
        while let Some(dir) = current {
            if dir.file_name() == Some(OsStr::new(".oms")) {
                if let Some(parent) = dir.parent()
                    && let Ok(root) = Root::open(parent)
                {
                    let _ = root.atomic_mutate::<()>(
                        &PathBuf::from(".oms/.gitignore"),
                        MutationOptions {
                            read: ByteReadOptions {
                                missing_ok: true,
                                max_bytes: MAX_PACKET,
                            },
                            create_parent: true,
                            preserve_existing: true,
                            ..MutationOptions::default()
                        },
                        |_| Ok(b"*\n".to_vec()),
                    );
                }
                break;
            }
            current = dir.parent();
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn archive(
        &self,
        repo: &Path,
        closed: &[u8],
        id: &[u8],
        stamp: &str,
        args: &[Vec<u8>],
        source: &[u8],
        reference: &mut dyn ReferenceOperation,
        interrupted: &AtomicBool,
        signal: Option<&AtomicI32>,
    ) -> Result<PathBuf> {
        if id.is_empty() || id.iter().any(|b| *b == b'/' || *b == 0) {
            return Err(Error::new(1, "unsafe task archive identity"));
        }
        let parent = self.relative.parent().unwrap_or(Path::new(""));
        let source_dir = directory(self.root.physical_root(), parent, false)?;
        match fs::mkdirat(&source_dir, "archive", Mode::from_raw_mode(0o700)) {
            Ok(()) => {
                sync_directory(&source_dir, "persist archive directory")?;
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(error("create archive directory", e)),
        }
        let archive = fs::openat(
            &source_dir,
            "archive",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| error("open archive directory", e))?;
        let mut suffix = 0u64;
        loop {
            checkpoint(interrupted, signal)?;
            let mut name = id.to_vec();
            name.extend_from_slice(
                format!(
                    "-{stamp}{}.md",
                    if suffix == 0 {
                        String::new()
                    } else {
                        format!("-{suffix}")
                    }
                )
                .as_bytes(),
            );
            let name = OsString::from_vec(name);
            let destination = self.path.parent().unwrap().join("archive").join(&name);
            if self.read()?.as_deref() != Some(closed) {
                return Err(Error::new(1, "task packet changed before archival"));
            }
            reference.validate(&ReferenceWrite {
                repo,
                target: &destination,
                operation: "agent_task_archive_unlocked",
                arguments: args,
                before: b"",
                proposed: closed,
                source_session: source,
            })?;
            match fs::statat(&archive, &name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => {
                    suffix = suffix
                        .checked_add(1)
                        .ok_or_else(|| Error::new(1, "archive collision limit"))?;
                    continue;
                }
                Err(rustix::io::Errno::NOENT) => {}
                Err(e) => return Err(error("inspect archive destination", e)),
            }
            let held = fs::openat(
                &source_dir,
                self.relative.file_name().unwrap(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| error("open archive source", e))?;
            let opened = fs::fstat(&held).map_err(|e| error("inspect open archive source", e))?;
            if opened.st_uid != rustix::process::getuid().as_raw()
                || opened.st_nlink != 1
                || fs::FileType::from_raw_mode(opened.st_mode) != fs::FileType::RegularFile
            {
                return Err(Error::new(1, "unsafe archive source"));
            }
            let mut file = File::from(held);
            let mut observed = Vec::new();
            file.by_ref()
                .take((MAX_PACKET + 1) as u64)
                .read_to_end(&mut observed)
                .map_err(|e| Error::io("read archive source", e))?;
            if observed != closed {
                return Err(Error::new(1, "archive source bytes changed"));
            }
            let named = fs::statat(
                &source_dir,
                self.relative.file_name().unwrap(),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|e| error("inspect archive source", e))?;
            let shape = |s: &fs::Stat| {
                (
                    s.st_dev,
                    s.st_ino,
                    s.st_mode,
                    s.st_uid,
                    s.st_nlink,
                    s.st_size,
                    s.st_mtime,
                    s.st_mtime_nsec,
                    s.st_ctime,
                    s.st_ctime_nsec,
                )
            };
            if shape(&opened) != shape(&named) {
                return Err(Error::new(1, "archive source generation changed"));
            }
            if named.st_uid != rustix::process::getuid().as_raw()
                || named.st_nlink != 1
                || fs::FileType::from_raw_mode(named.st_mode) != fs::FileType::RegularFile
            {
                return Err(Error::new(1, "unsafe archive source"));
            }
            // Reopening the anchored ancestry detects replaced directory names
            // before the descriptor-relative no-replace rename.
            let source_again = directory(self.root.physical_root(), parent, false)?;
            let a = fs::fstat(&source_dir).map_err(|e| error("inspect task directory", e))?;
            let b = fs::fstat(&source_again).map_err(|e| error("inspect task directory", e))?;
            let archive_again = fs::statat(&source_dir, "archive", AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| error("inspect archive directory", e))?;
            let archive_info =
                fs::fstat(&archive).map_err(|e| error("inspect archive directory", e))?;
            if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino)
                || (archive_again.st_dev, archive_again.st_ino)
                    != (archive_info.st_dev, archive_info.st_ino)
            {
                return Err(Error::new(1, "task directory changed during archival"));
            }
            match fs::renameat_with(
                &source_dir,
                self.relative.file_name().unwrap(),
                &archive,
                &name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    let published = fs::statat(&archive, &name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|e| error("archive published; inspect generation", e))?;
                    if (published.st_dev, published.st_ino) != (opened.st_dev, opened.st_ino) {
                        return Err(Error::new(
                            1,
                            "archive published with changed generation; reconcile before retry",
                        ));
                    }
                    sync_directory(&archive, "archive visible; directory durability uncertain")?;
                    sync_directory(
                        &source_dir,
                        "archive visible; source directory durability uncertain",
                    )?;
                    return Ok(destination);
                }
                Err(rustix::io::Errno::EXIST) => {
                    suffix = suffix
                        .checked_add(1)
                        .ok_or_else(|| Error::new(1, "archive collision limit"))?
                }
                Err(e) => {
                    return Err(error(
                        "archive publication uncertain; reconcile before retry",
                        e,
                    ));
                }
            }
        }
    }
}
fn directory(root: &Path, relative: &Path, create: bool) -> Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = fs::open(root, flags, Mode::empty()).map_err(|e| error("open storage root", e))?;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            if part == Component::CurDir {
                continue;
            }
            return Err(Error::new(1, "unsafe storage ancestry"));
        };
        if create {
            match fs::mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(error("create storage directory", e)),
            }
        }
        fd = fs::openat(&fd, name, flags, Mode::empty())
            .map_err(|e| error("open storage ancestry", e))?;
    }
    Ok(fd)
}
pub(crate) fn read(root: &Root, path: &Path, cap: usize) -> Result<Option<Vec<u8>>> {
    match root.read_path(
        path,
        cap,
        Policy {
            single_link: true,
            owner: Some(rustix::process::getuid().as_raw()),
            max_components: None,
        },
    ) {
        Ok(v) => Ok(Some(v)),
        Err(oms_safe_fs::Error::Missing) => Ok(None),
        Err(e) => Err(error("read canonical task record", e)),
    }
}
pub(crate) fn locked<T>(
    path: &Path,
    interrupted: &AtomicBool,
    signal: Option<&AtomicI32>,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock = oms_lock::Lock::for_file(
        path,
        oms_lock::Settings::from_env().map_err(|e| error("lock settings", e))?,
    )
    .map_err(|e| error("task lock", e))?;
    let guard = lock
        .acquire_with_cancel(|| interrupted.load(Ordering::Relaxed))
        .map_err(|e| {
            if interrupted.load(Ordering::Relaxed) {
                Error::new(
                    crate::process::interrupted_code(signal),
                    "interrupted while acquiring task lock",
                )
            } else {
                error("task lock", e)
            }
        })?;
    checkpoint(interrupted, signal)?;
    guard.check().map_err(|e| error("task lock authority", e))?;
    let result = action();
    let released = guard.release().map_err(|e| error("release task lock", e));
    match (result, released) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e),
        (Ok(v), Ok(())) => Ok(v),
    }
}
