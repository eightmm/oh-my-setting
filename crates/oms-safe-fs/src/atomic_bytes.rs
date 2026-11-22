use super::{Root, directory_flags, identity, safe_registry_dir, same_registry_dir};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Path-free byte storage failures; no owner restriction is imposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteError {
    InvalidPath,
    Missing,
    Unsafe,
    /// An observed change, or inability to re-establish root/ancestor identity.
    /// This is neither proof of another actor's mutation nor retry authority.
    Changed,
    InputTooLarge,
    OutputTooLarge,
    NotWritable,
    ExistenceMismatch,
    Io(Errno),
}

/// Zero permits only an empty body. Missing parents/leaves read as empty only
/// when requested; unsafe components are always refused.
#[derive(Debug, Clone, Copy)]
pub struct ByteReadOptions {
    pub missing_ok: bool,
    pub max_bytes: usize,
}
impl Default for ByteReadOptions {
    fn default() -> Self {
        Self {
            missing_ok: false,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MutationOptions {
    pub read: ByteReadOptions,
    /// Create missing parents with mode 0700 (subject to umask), persisting
    /// each new directory name. Refusals may leave those directories behind.
    pub create_parent: bool,
    pub max_output: usize,
    /// An existing bounded input is returned without calling the transform.
    pub preserve_existing: bool,
    /// Checked after the input read, before preservation or transformation.
    /// `Some(false)` still requires `read.missing_ok` to create a missing file.
    pub expected_exists: Option<bool>,
}
impl Default for MutationOptions {
    fn default() -> Self {
        Self {
            read: ByteReadOptions::default(),
            create_parent: false,
            max_output: 16 * 1024 * 1024,
            preserve_existing: false,
            expected_exists: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationDisposition {
    Created,
    Replaced,
    /// No new fsync. This cannot reconcile durability after a prior error.
    Unchanged,
    Preserved,
}

/// The returned bytes are the old body for early returns and the new body
/// after publication. Unchanged and preserved files are not fsynced anew.
#[derive(Debug, PartialEq, Eq)]
pub struct MutationOutcome {
    pub bytes: Vec<u8>,
    pub disposition: MutationDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationPhase {
    /// No destination replacement was attempted. Parent creation may remain.
    BeforePublication,
    /// Rename returned a raw `ByteError::Io(errno)`, including ENOENT rather
    /// than ordinary Missing. Remote filesystems may have applied it; callers
    /// must reconcile state rather than blindly retry.
    PublicationUncertain,
    /// Rename succeeded but persisting the directory failed. New bytes are
    /// visible; their survival after a crash has not been confirmed.
    Published,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MutationError<E> {
    Storage {
        phase: MutationPhase,
        error: ByteError,
    },
    /// The callback refused before any destination replacement was attempted.
    Transform(E),
}
impl<E> From<ByteError> for MutationError<E> {
    // Only prepublication paths may use this implicit phase conversion.
    fn from(error: ByteError) -> Self {
        Self::Storage {
            phase: MutationPhase::BeforePublication,
            error,
        }
    }
}

fn io_error(error: Errno) -> ByteError {
    match error {
        Errno::NOENT => ByteError::Missing,
        Errno::LOOP | Errno::NOTDIR => ByteError::Unsafe,
        other => ByteError::Io(other),
    }
}

fn observed_directory_error(error: Errno) -> ByteError {
    match error {
        Errno::NOENT => ByteError::Changed,
        other => io_error(other),
    }
}

fn regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile && stat.st_nlink == 1
}

// Matches the legacy mutation snapshot, including ctime at nanosecond precision.
fn unchanged(before: &Stat, after: &Stat) -> bool {
    identity(before, after)
        && before.st_size == after.st_size
        && before.st_mtime == after.st_mtime
        && before.st_mtime_nsec == after.st_mtime_nsec
        && before.st_ctime == after.st_ctime
        && before.st_ctime_nsec == after.st_ctime_nsec
        && after.st_nlink == 1
}

fn directory_sync(result: Result<(), Errno>) -> Result<(), ByteError> {
    match result {
        Ok(()) | Err(Errno::BADF | Errno::INVAL | Errno::NOTSUP) => Ok(()),
        Err(error) => Err(ByteError::Io(error)),
    }
}

// Normalize lexically, never resolving a supplied in-repository component.
fn parts(root: &Root, path: &Path) -> Result<Vec<OsString>, ByteError> {
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(ByteError::InvalidPath);
    }
    let absolute = root.path.join(path);
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::Normal(_) => normalized.push(component),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => return Err(ByteError::InvalidPath),
        }
    }
    let relative = normalized
        .strip_prefix(&root.path)
        .map_err(|_| ByteError::InvalidPath)?;
    let parts: Vec<_> = relative
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    if parts.is_empty() {
        return Err(ByteError::InvalidPath);
    }
    Ok(parts)
}

struct Anchor<'a> {
    root: &'a Root,
    parts: Vec<OsString>,
    dirs: Vec<OwnedFd>,
    dir_stats: Vec<Stat>,
}
impl<'a> Anchor<'a> {
    fn open(
        root: &'a Root,
        path: &Path,
        create: bool,
        hook: &mut impl FnMut(Stage) -> Result<(), Errno>,
    ) -> Result<Self, ByteError> {
        let mut anchor = Self {
            root,
            parts: parts(root, path)?,
            dirs: Vec::new(),
            dir_stats: Vec::new(),
        };
        anchor.revalidate()?;
        for index in 0..anchor.parts.len() - 1 {
            let parent = anchor.parent();
            let name = &anchor.parts[index];
            let named = match fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(Errno::NOENT) if create => {
                    anchor.revalidate()?;
                    hook(Stage::BeforeMkdir).map_err(observed_directory_error)?;
                    let created = match fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => true,
                        Err(Errno::EXIST) => false,
                        Err(error) => return Err(observed_directory_error(error)),
                    };
                    let stat = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(observed_directory_error)?;
                    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                        return Err(ByteError::Unsafe);
                    }
                    if created {
                        directory_sync(
                            hook(Stage::BeforeParentSync).and_then(|()| fs::fsync(parent)),
                        )?;
                    }
                    stat
                }
                Err(error) => {
                    anchor.revalidate()?;
                    return Err(io_error(error));
                }
            };
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || (root.private_ancestry.is_some() && !safe_registry_dir(&named))
            {
                return Err(ByteError::Unsafe);
            }
            let dir = hook(Stage::BeforeDirectoryOpen)
                .and_then(|()| fs::openat(parent, name, directory_flags(), Mode::empty()))
                .map_err(observed_directory_error)?;
            let opened = fs::fstat(&dir).map_err(ByteError::Io)?;
            if !identity(&named, &opened)
                || (root.private_ancestry.is_some() && !same_registry_dir(&named, &opened))
            {
                return Err(ByteError::Changed);
            }
            anchor.dirs.push(dir);
            anchor.dir_stats.push(opened);
        }
        anchor.revalidate()?;
        Ok(anchor)
    }

    fn parent(&self) -> &OwnedFd {
        self.dirs.last().unwrap_or(&self.root.fd)
    }

    fn name(&self) -> &OsStr {
        self.parts.last().unwrap()
    }

    fn revalidate(&self) -> Result<(), ByteError> {
        self.root
            .revalidate_private()
            .map_err(|_| ByteError::Changed)?;
        let root = fs::open(&self.root.path, directory_flags(), Mode::empty())
            .map_err(|_| ByteError::Changed)?;
        if !identity(&self.root.initial, &fs::fstat(root).map_err(ByteError::Io)?) {
            return Err(ByteError::Changed);
        }
        for (index, dir) in self.dirs.iter().enumerate() {
            let parent = if index == 0 {
                &self.root.fd
            } else {
                &self.dirs[index - 1]
            };
            let named = fs::statat(parent, &self.parts[index], AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| ByteError::Changed)?;
            let opened = fs::fstat(dir).map_err(ByteError::Io)?;
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || !identity(&named, &opened)
                || (self.root.private_ancestry.is_some()
                    && (!same_registry_dir(&self.dir_stats[index], &named)
                        || !same_registry_dir(&self.dir_stats[index], &opened)))
            {
                return Err(ByteError::Changed);
            }
        }
        Ok(())
    }

    fn valid_leaf(&self, stat: &Stat) -> bool {
        regular(stat)
            && (self.root.private_ancestry.is_none()
                || (stat.st_uid == rustix::process::getuid().as_raw()
                    && stat.st_mode & 0o077 == 0
                    && stat.st_mode & 0o7000 == 0))
    }

    fn leaf_unchanged(&self, a: &Stat, b: &Stat) -> bool {
        unchanged(a, b)
            && (self.root.private_ancestry.is_none()
                || (self.valid_leaf(b) && a.st_uid == b.st_uid && a.st_mode == b.st_mode))
    }

    fn named(&self) -> Result<Option<Stat>, ByteError> {
        match fs::statat(self.parent(), self.name(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if self.valid_leaf(&stat) => Ok(Some(stat)),
            Ok(_) => Err(ByteError::Unsafe),
            Err(Errno::NOENT) => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    fn check_leaf(&self, before: Option<&Stat>) -> Result<(), ByteError> {
        match (before, self.named()?) {
            (None, None) => Ok(()),
            (Some(before), Some(after)) if self.leaf_unchanged(before, &after) => Ok(()),
            _ => Err(ByteError::Changed),
        }
    }

    fn read(
        &self,
        options: ByteReadOptions,
        hook: &mut impl FnMut(Stage) -> Result<(), Errno>,
    ) -> Result<(Vec<u8>, Option<Stat>), ByteError> {
        let named = self.named()?;
        hook(Stage::BeforeLeafOpen).map_err(io_error)?;
        let fd = match fs::openat(
            self.parent(),
            self.name(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if named.is_some() => return Err(ByteError::Changed),
            Err(Errno::NOENT) if options.missing_ok && named.is_none() => {
                self.revalidate()?;
                return Ok((Vec::new(), None));
            }
            Err(error) => return Err(io_error(error)),
        };
        let before = fs::fstat(&fd).map_err(io_error)?;
        if !self.valid_leaf(&before) {
            return Err(ByteError::Unsafe);
        }
        if !named
            .as_ref()
            .is_some_and(|stat| self.leaf_unchanged(stat, &before))
        {
            return Err(ByteError::Changed);
        }
        self.check_leaf(Some(&before))?;
        self.revalidate()?;
        hook(Stage::BeforeRead).map_err(io_error)?;
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        let mut buffer = [0; 65536];
        loop {
            let remaining = options.max_bytes.saturating_sub(bytes.len());
            let count = buffer.len().min(remaining.saturating_add(1));
            let count = match file.read(&mut buffer[..count]) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(ByteError::Io(
                        Errno::from_io_error(&error).unwrap_or(Errno::IO),
                    ));
                }
            };
            if count == 0 {
                break;
            }
            if count > remaining {
                return Err(ByteError::InputTooLarge);
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        hook(Stage::AfterRead).map_err(io_error)?;
        if !self.leaf_unchanged(&before, &fs::fstat(&file).map_err(io_error)?) {
            return Err(ByteError::Changed);
        }
        self.check_leaf(Some(&before))?;
        self.revalidate()?;
        Ok((bytes, Some(before)))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    BeforeMkdir,
    BeforeParentSync,
    BeforeDirectoryOpen,
    BeforeLeafOpen,
    BeforeRead,
    AfterRead,
    BeforeProbe,
    BeforeTempOpen,
    AfterTempOpen,
    BeforeWrite,
    BeforeFileSync,
    BeforePublish,
    Rename,
    BeforeDirectorySync,
}

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp<'a> {
    directory: &'a OwnedFd,
    name: OsString,
    file: std::fs::File,
    initial: Stat,
    published: bool,
}
impl<'a> Temp<'a> {
    fn create(anchor: &'a Anchor<'_>) -> Result<Self, ByteError> {
        let mut name = OsString::from(".");
        name.push(anchor.name());
        name.push(format!(
            ".tmp.{}.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        Self::create_named(anchor, name)
    }

    fn create_named(anchor: &'a Anchor<'_>, name: OsString) -> Result<Self, ByteError> {
        let fd = fs::openat(
            anchor.parent(),
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io_error)?;
        let initial = fs::fstat(&fd).map_err(io_error)?;
        Ok(Self {
            directory: anchor.parent(),
            name,
            file: std::fs::File::from(fd),
            initial,
            published: false,
        })
    }

    fn owns_name(&self) -> bool {
        fs::statat(self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| regular(&stat) && identity(&self.initial, &stat))
    }
}
impl Drop for Temp<'_> {
    fn drop(&mut self) {
        if !self.published && self.owns_name() {
            let _ = fs::unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}

impl Root {
    /// Read bounded bytes with single-link, regular-file and ancestry checks.
    /// Paths are lexically normalized inside the physical root; absolute paths
    /// must use its captured physical spelling. Adapters must rebase absolute
    /// logical-root aliases before calling, or use relative paths when opening
    /// the root through a benign ancestry alias. No UTF-8 or owner is required.
    /// A leaf observed present but missing at open returns Changed regardless
    /// of missing_ok. Initially absent input retains the requested missing policy.
    /// An ancestor disappearing after observation also returns Changed; failed
    /// descriptor validation never becomes an allowed-empty read.
    pub fn read_no_follow(
        &self,
        path: &Path,
        options: ByteReadOptions,
    ) -> Result<Vec<u8>, ByteError> {
        self.read_bytes_with(path, options, |_| Ok(()))
    }

    fn read_bytes_with(
        &self,
        path: &Path,
        options: ByteReadOptions,
        mut hook: impl FnMut(Stage) -> Result<(), Errno>,
    ) -> Result<Vec<u8>, ByteError> {
        let anchor = match Anchor::open(self, path, false, &mut hook) {
            Ok(anchor) => anchor,
            Err(ByteError::Missing) if options.missing_ok => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        anchor.read(options, &mut hook).map(|(bytes, _)| bytes)
    }

    /// Copy-on-write byte mutation. Callers MUST hold the canonical external
    /// oms-lock across the entire read-modify-write transaction; rename does
    /// not serialize updates. Snapshot checks only detect observed races.
    /// Paths follow [`Self::read_no_follow`], including adapter alias rebasing.
    ///
    /// Missing input is empty only when requested. Existence is checked before
    /// preserve-existing, then the transform runs once. An existing identical
    /// body returns before output-bound and writable checks, like the legacy
    /// writer. Callback errors never replace the destination.
    /// Unchanged after a prior publication error is not durable reconciliation:
    /// it performs no new fsync. Adapters need an explicit recovery path rather
    /// than blindly retrying the same mutation.
    ///
    /// A changed existing file needs write bits and a non-truncating write
    /// probe. Unlike the legacy mutation, replacement strips setuid/setgid when
    /// best-effort copying permission bits; ordinary permissions and sticky are
    /// retained. Ownership follows new-file creation (effective user and host
    /// group-inheritance rules), never an implicit chown. New files use 0600
    /// subject to umask.
    /// If copying permissions fails, the creation mode remains (0600 subject
    /// to umask). Adapters must verify any required mode postcondition after
    /// publication; this primitive does not make chmod failure fatal.
    /// The temporary body is fsynced before rename, then the directory is
    /// fsynced where supported.
    ///
    /// Before-publication failures preserve the destination, but may leave
    /// created parents. A kill may leave an exclusive temporary file; subsequent
    /// calls do not delete it. Cleanup checks identity and only unlinks this
    /// operation's own single-link temporary name. Publication errors and
    /// published durability errors MUST be reconciled before any retry.
    pub fn atomic_mutate<E>(
        &self,
        path: &Path,
        options: MutationOptions,
        transform: impl FnOnce(&[u8]) -> Result<Vec<u8>, E>,
    ) -> Result<MutationOutcome, MutationError<E>> {
        self.mutate_with(path, options, transform, |_| Ok(()))
    }

    fn mutate_with<E>(
        &self,
        path: &Path,
        options: MutationOptions,
        transform: impl FnOnce(&[u8]) -> Result<Vec<u8>, E>,
        mut hook: impl FnMut(Stage) -> Result<(), Errno>,
    ) -> Result<MutationOutcome, MutationError<E>> {
        let anchor = Anchor::open(self, path, options.create_parent, &mut hook)?;
        let (old, before) = anchor.read(options.read, &mut hook)?;
        if options
            .expected_exists
            .is_some_and(|exists| exists != before.is_some())
        {
            return Err(ByteError::ExistenceMismatch.into());
        }
        if before.is_some() && options.preserve_existing {
            return Ok(MutationOutcome {
                bytes: old,
                disposition: MutationDisposition::Preserved,
            });
        }
        let new = transform(&old).map_err(MutationError::Transform)?;
        if before.is_some() && new == old {
            return Ok(MutationOutcome {
                bytes: old,
                disposition: MutationDisposition::Unchanged,
            });
        }
        if new.len() > options.max_output {
            return Err(ByteError::OutputTooLarge.into());
        }
        if let Some(before) = &before {
            if before.st_mode & 0o222 == 0 {
                return Err(ByteError::NotWritable.into());
            }
            hook(Stage::BeforeProbe).map_err(io_error)?;
            let probe = fs::openat(
                anchor.parent(),
                anchor.name(),
                OFlags::WRONLY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC
                    | OFlags::NOCTTY,
                Mode::empty(),
            )
            .map_err(io_error)?;
            let probed = fs::fstat(probe).map_err(io_error)?;
            if !anchor.valid_leaf(&probed) || !anchor.leaf_unchanged(before, &probed) {
                return Err(ByteError::Changed.into());
            }
        }
        anchor.revalidate()?;
        hook(Stage::BeforeTempOpen).map_err(io_error)?;
        let mut temp = Temp::create(&anchor)?;
        hook(Stage::AfterTempOpen).map_err(io_error)?;
        hook(Stage::BeforeWrite).map_err(io_error)?;
        temp.file
            .write_all(&new)
            .map_err(|error| ByteError::Io(Errno::from_io_error(&error).unwrap_or(Errno::IO)))?;
        if let Some(before) = &before {
            let _ = fs::fchmod(&temp.file, Mode::from_raw_mode(before.st_mode & 0o1777));
        }
        hook(Stage::BeforeFileSync).map_err(io_error)?;
        fs::fsync(&temp.file).map_err(io_error)?;
        hook(Stage::BeforePublish).map_err(io_error)?;
        anchor.revalidate()?;
        anchor.check_leaf(before.as_ref())?;
        if !temp.owns_name()
            || (self.private_ancestry.is_some()
                && !anchor.valid_leaf(&fs::fstat(&temp.file).map_err(io_error)?))
        {
            return Err(ByteError::Changed.into());
        }
        // From<ByteError> always means BeforePublication. Every fallible operation
        // from here must map its explicit phase; never add a bare ? after rename.
        hook(Stage::Rename)
            .and_then(|()| {
                fs::renameat(anchor.parent(), &temp.name, anchor.parent(), anchor.name())
            })
            .map_err(|error| MutationError::Storage {
                phase: MutationPhase::PublicationUncertain,
                error: ByteError::Io(error),
            })?;
        temp.published = true;
        directory_sync(hook(Stage::BeforeDirectorySync).and_then(|()| fs::fsync(anchor.parent())))
            .map_err(|error| MutationError::Storage {
                phase: MutationPhase::Published,
                error,
            })?;
        Ok(MutationOutcome {
            bytes: new,
            disposition: if before.is_some() {
                MutationDisposition::Replaced
            } else {
                MutationDisposition::Created
            },
        })
    }
}

#[cfg(test)]
mod tests;
