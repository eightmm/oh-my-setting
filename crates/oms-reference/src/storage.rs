//! Named namespace checks accompany descriptor-relative reads and mutations.
//! Same-owner group write is compatible; world write is never accepted.
use crate::{Result, V, encoded};
use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use std::{
    fs::{self, File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

pub const MAX_STREAM: usize = 32 * 1024 * 1024;
pub const MAX_OBJECT: usize = 16 * 1024 * 1024;
pub fn identity(info: &Metadata) -> V {
    V::Arr(vec![
        V::Num(info.dev().to_string()),
        V::Num(info.ino().to_string()),
    ])
}
pub fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}
pub fn generation(left: &Metadata, right: &Metadata) -> bool {
    same(left, right)
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.nlink() == right.nlink()
        && left.size() == right.size()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
fn own(info: &Metadata) -> bool {
    info.uid() == rustix::process::getuid().as_raw() && info.mode() & 0o002 == 0
}
pub fn private_directory(path: &Path) -> Result<Metadata> {
    let info = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !info.is_dir() || !own(&info) || info.mode() & 0o777 != 0o700 {
        return Err("capture generation directory is not private".into());
    }
    Ok(info)
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            part => result.push(part.as_os_str()),
        }
    }
    Ok(result)
}

/// Captures every named directory and the first absent component. Rechecking
/// absence must also re-establish the namespace that proved it absent.
pub struct Namespace {
    entries: Vec<(PathBuf, Metadata)>,
    missing: Option<PathBuf>,
}
impl Namespace {
    pub fn snapshot(root: &Path, target: &Path) -> Result<Self> {
        let relative = target
            .strip_prefix(root)
            .map_err(|_| "path leaves physical repository")?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err("unsafe namespace path".into());
        }
        let mut current = root.to_owned();
        let mut entries = Vec::new();
        let mut parts = relative.components().collect::<Vec<_>>();
        parts.pop();
        for index in 0..=parts.len() {
            let info = match fs::symlink_metadata(&current) {
                Ok(info) => info,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Self {
                        entries,
                        missing: Some(current),
                    });
                }
                Err(e) => return Err(e.to_string()),
            };
            if !info.is_dir() || !own(&info) {
                return Err("unsafe or foreign reference namespace".into());
            }
            entries.push((current.clone(), info));
            if let Some(part) = parts.get(index) {
                current.push(part.as_os_str());
            }
        }
        Ok(Self {
            entries,
            missing: None,
        })
    }
    pub fn check(&self) -> Result<()> {
        for (path, old) in &self.entries {
            let new = fs::symlink_metadata(path).map_err(|_| "named namespace disappeared")?;
            if !new.is_dir() || !own(&new) || !same(old, &new) {
                return Err("named namespace changed".into());
            }
        }
        if let Some(path) = &self.missing {
            match fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("absent namespace changed".into()),
            }
        }
        Ok(())
    }
}

pub fn regular(path: &Path, maximum: usize, links: u64) -> Result<(Vec<u8>, Metadata)> {
    let logical = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let logical_parent = logical.parent().ok_or("input has no parent")?;
    let physical_parent = fs::canonicalize(logical_parent).map_err(|e| e.to_string())?;
    let physical = physical_parent.join(logical.file_name().ok_or("input has no name")?);
    let path = physical.as_path();
    let parent = physical_parent.as_path();
    let parent_info = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if !parent_info.is_dir() {
        return Err("capture input parent is redirected".into());
    }
    let parent_file: File = rfs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?
    .into();
    if !same(
        &parent_info,
        &parent_file.metadata().map_err(|e| e.to_string())?,
    ) {
        return Err("capture input parent changed".into());
    }
    let before = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let mut file: File = rfs::openat(
        &parent_file,
        path.file_name().ok_or("input has no name")?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?
    .into();
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file()
        || !own(&opened)
        || opened.nlink() != links
        || opened.size() > maximum as u64
        || !generation(&before, &opened)
    {
        return Err("unsafe or oversized capture input".into());
    }
    let mut raw = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let named = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let named_parent = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if raw.len() > maximum
        || raw.len() as u64 != opened.size()
        || !generation(&opened, &after)
        || !generation(&after, &named)
        || !named_parent.is_dir()
        || !same(&parent_info, &named_parent)
        || !same(
            &parent_info,
            &fs::metadata(logical_parent).map_err(|e| e.to_string())?,
        )
        || !generation(
            &after,
            &fs::symlink_metadata(&logical).map_err(|e| e.to_string())?,
        )
    {
        return Err("capture input changed while reading".into());
    }
    Ok((raw, after))
}

pub fn read_repo(root: &Path, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
    let target = root.join(relative);
    let snapshot = Namespace::snapshot(root, &target)?;
    let before = match fs::symlink_metadata(&target) {
        Ok(info) if info.is_file() && own(&info) && info.nlink() == 1 => Some(info),
        Ok(_) => return Err("unsafe or foreign repository input".into()),
        Err(e) if missing && e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let opened =
        oms_safe_fs::Root::open(root).map_err(|e| format!("repository input root: {e:?}"))?;
    let raw = opened
        .read_no_follow(
            relative,
            oms_safe_fs::ByteReadOptions {
                missing_ok: missing,
                max_bytes: maximum,
            },
        )
        .map_err(|e| format!("repository input: {e:?}"))?;
    snapshot.check()?;
    match (before, fs::symlink_metadata(&target)) {
        (Some(before), Ok(after)) if generation(&before, &after) => {}
        (None, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("repository input changed".into()),
    }
    Ok(raw)
}
pub fn sync_directory(path: &Path) -> Result<()> {
    let fd = rfs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    match rfs::fsync(fd) {
        Ok(())
        | Err(rustix::io::Errno::INVAL | rustix::io::Errno::BADF | rustix::io::Errno::NOTSUP) => {
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
/// Append one bounded row, without rewriting or rereading the ledger. The
/// caller holds its canonical oms-lock and has validated a fresh snapshot.
pub(crate) fn append(
    root: &Path,
    relative: &Path,
    row: &V,
    expected: Option<&Metadata>,
) -> Result<Metadata> {
    let data = encoded(row)?;
    if data.len() > 1024 * 1024 || data[..data.len() - 1].contains(&b'\n') || data.contains(&0) {
        return Err("invalid capture journal row".into());
    }
    let path = root.join(relative);
    let namespace = Namespace::snapshot(root, &path)?;
    let parent = path.parent().ok_or("journal has no parent")?;
    let directory: File = rfs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?
    .into();
    namespace.check()?;
    let old = match fs::symlink_metadata(&path) {
        Ok(info) => Some(info),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    match (expected, old.as_ref()) {
        (Some(expected), Some(actual)) if generation(expected, actual) => {}
        (None, None) => {}
        _ => return Err("capture journal changed before append".into()),
    }
    let mut flags =
        OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    if old.is_none() {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let mut file: File = rfs::openat(
        &directory,
        path.file_name().ok_or("journal has no name")?,
        flags,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| e.to_string())?
    .into();
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file()
        || !own(&opened)
        || opened.nlink() != 1
        || opened.size() + data.len() as u64 > 16 * 1024 * 1024
        || old.as_ref().is_some_and(|v| !generation(v, &opened))
    {
        return Err("unsafe or oversized capture journal".into());
    }
    namespace.check()?;
    if !same(
        &opened,
        &fs::symlink_metadata(&path).map_err(|e| e.to_string())?,
    ) {
        return Err("capture journal replaced".into());
    }
    if file.write(&data).map_err(|e| e.to_string())? != data.len() {
        return Err("capture journal append was short; publication uncertain".into());
    }
    file.sync_all()
        .map_err(|e| format!("capture append durability uncertain: {e}"))?;
    namespace.check()?;
    let after = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !same(&opened, &after)
        || after.nlink() != 1
        || !own(&after)
        || after.size() != opened.size() + data.len() as u64
        || !generation(&after, &file.metadata().map_err(|e| e.to_string())?)
    {
        return Err("capture journal changed during append".into());
    }
    sync_directory(parent)?;
    Ok(after)
}

/// The shared primitive performs fsync, namespace checks and leaf CAS. This
/// wrapper additionally requires owned, non-world-writable bytes before/after.
pub(crate) fn replace(
    root: &Path,
    relative: &Path,
    before: &[u8],
    proposed: &[u8],
    missing: bool,
    expected: Option<&Metadata>,
) -> Result<()> {
    use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
    let path = root.join(relative);
    let snapshot = Namespace::snapshot(root, &path)?;
    if let Ok(info) = fs::symlink_metadata(&path)
        && !own(&info)
    {
        return Err("foreign or world-writable publication".into());
    }
    let opened = Root::open(root).map_err(|e| format!("publication root: {e:?}"))?;
    opened
        .atomic_mutate(
            relative,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: missing,
                    max_bytes: MAX_STREAM,
                },
                create_parent: false,
                max_output: MAX_STREAM,
                expected_exists: Some(expected.is_some()),
                ..Default::default()
            },
            |raw| {
                if raw != before {
                    return Err("publication input changed".to_string());
                }
                match (expected, fs::symlink_metadata(&path)) {
                    (Some(before), Ok(after)) if generation(before, &after) => {}
                    (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("publication generation changed".into()),
                }
                snapshot.check()?;
                Ok(proposed.to_vec())
            },
        )
        .map_err(|e| format!("publication failed: {e:?}"))?;
    snapshot.check()?;
    regular(&path, MAX_STREAM, 1)?;
    Ok(())
}

pub(crate) fn ensure_parent(root: &Path, relative: &Path) -> Result<()> {
    let initial = Namespace::snapshot(root, &root.join(".namespace"))?;
    let mut directory: File = rfs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?
    .into();
    if !same(
        &fs::symlink_metadata(root).map_err(|e| e.to_string())?,
        &directory.metadata().map_err(|e| e.to_string())?,
    ) {
        return Err("publication root changed before open".into());
    }
    initial.check()?;
    let mut current = root.to_owned();
    let parts = relative.parent().ok_or("path has no parent")?;
    for part in parts.components() {
        let Component::Normal(name) = part else {
            return Err("unsafe parent component".into());
        };
        let snapshot = Namespace::snapshot(root, &current.join(".namespace"))?;
        match rfs::mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => sync_directory(&current)?,
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.to_string()),
        }
        snapshot.check()?;
        let stat =
            rfs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| e.to_string())?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory
            || stat.st_uid != rustix::process::getuid().as_raw()
            || stat.st_mode & 0o002 != 0
        {
            return Err("unsafe publication parent".into());
        }
        let next = rfs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| e.to_string())?;
        let opened = rfs::fstat(&next).map_err(|e| e.to_string())?;
        if opened.st_dev != stat.st_dev || opened.st_ino != stat.st_ino {
            return Err("publication parent replaced before open".into());
        }
        directory = next.into();
        current.push(name);
        if !same(
            &fs::symlink_metadata(&current).map_err(|e| e.to_string())?,
            &directory.metadata().map_err(|e| e.to_string())?,
        ) {
            return Err("publication parent changed after open".into());
        }
        initial.check()?;
        Namespace::snapshot(root, &current.join(".namespace"))?.check()?;
    }
    initial.check()?;
    Ok(())
}

pub(crate) fn acquire(
    path: &Path,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<oms_lock::Guard> {
    let settings = oms_lock::Settings::from_env().map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + settings.timeout;
    let lock = oms_lock::Lock::for_file(path, settings).map_err(|e| e.to_string())?;
    loop {
        crate::checkpoint(interrupted)?;
        match lock.try_acquire() {
            Ok(guard) => return Ok(guard),
            Err(oms_lock::Error::Busy) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    let mut value = vec![0; bytes];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut value))
        .map_err(|e| e.to_string())?;
    Ok(value.iter().map(|v| format!("{v:02x}")).collect())
}

pub(crate) fn create_private(root: &Path, name: &Path, raw: &[u8]) -> Result<Metadata> {
    private_directory(root)?;
    let snapshot = Namespace::snapshot(root, &root.join(name))?;
    let directory = rfs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let mut file: File = rfs::openat(
        &directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| e.to_string())?
    .into();
    let before = file.metadata().map_err(|e| e.to_string())?;
    snapshot.check()?;
    file.write_all(raw).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    snapshot.check()?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let named = fs::symlink_metadata(root.join(name)).map_err(|e| e.to_string())?;
    if !same(&before, &after)
        || !generation(&after, &named)
        || !after.is_file()
        || after.nlink() != 1
        || !own(&after)
    {
        return Err("private publication changed".into());
    }
    sync_directory(root)?;
    Ok(after)
}

pub(crate) fn link(
    source: &Path,
    target_root: &Path,
    target_relative: &Path,
    expected: &Metadata,
) -> Result<()> {
    let target = target_root.join(target_relative);
    let snapshot = Namespace::snapshot(target_root, &target)?;
    let source_parent = source.parent().ok_or("anchor has no parent")?;
    let source_snapshot = Namespace::snapshot(source_parent, source)?;
    let source_dir = rfs::open(
        source_parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let parent = target.parent().ok_or("publication has no parent")?;
    let target_dir = rfs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let before = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if !generation(&before, expected) || !before.is_file() || before.nlink() != 1 {
        return Err("generation anchor changed before link".into());
    }
    snapshot.check()?;
    source_snapshot.check()?;
    rfs::linkat(
        &source_dir,
        source.file_name().ok_or("anchor has no name")?,
        &target_dir,
        target.file_name().ok_or("publication has no name")?,
        AtFlags::empty(),
    )
    .map_err(|e| e.to_string())?;
    snapshot.check()?;
    source_snapshot.check()?;
    let a = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    let b = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
    if !same(&a, expected)
        || !same(&b, expected)
        || a.nlink() != 2
        || !own(&a)
        || !a.is_file()
        || !b.is_file()
    {
        return Err("generation publication changed".into());
    }
    sync_directory(parent)?;
    if parent != source_parent {
        sync_directory(source_parent)?;
    }
    Ok(())
}
