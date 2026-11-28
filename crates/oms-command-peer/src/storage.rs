//! Locked, descriptor-anchored state. A transaction publishes either complete
//! old bytes or complete new bytes; a killed writer can leave its private temp.
use crate::{Error, Result, V, check, fail};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub const MAX_FILE: usize = 64 * 1024 * 1024;
pub const MAX_ROW: usize = 1024 * 1024;
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        fail("path must be repository-relative without traversal")
    } else {
        Ok(())
    }
}
pub fn root(path: &Path) -> Result<Root> {
    Root::open(path).map_err(|_| Error::new(2, "unsafe or unavailable repository"))
}

/// Bytes plus the (device, inode) of the descriptor they were read from.
pub(crate) type Snapshot = (Vec<u8>, Option<(u64, u64)>);

#[allow(clippy::unnecessary_cast)]
pub(crate) fn read_snapshot(
    repo: &Path,
    path: &Path,
    missing: bool,
    jsonl: bool,
) -> Result<Snapshot> {
    root(repo)?
        .read_no_follow_retrying_stat(
            path,
            ByteReadOptions {
                missing_ok: missing,
                max_bytes: MAX_FILE,
            },
            jsonl,
        )
        .map(|(bytes, stat)| (bytes, stat.map(|s| (s.st_dev as u64, s.st_ino as u64))))
        .map_err(|e| {
            Error::new(
                2,
                format!("unsafe, changed, oversized or unavailable file: {e:?}"),
            )
        })
}

pub fn read(repo: &Path, path: &Path, missing: bool) -> Result<Vec<u8>> {
    read_snapshot(repo, path, missing, false).map(|(bytes, _)| bytes)
}
pub(crate) fn read_jsonl(repo: &Path, path: &Path, missing: bool) -> Result<Vec<u8>> {
    read_snapshot(repo, path, missing, true).map(|(bytes, _)| bytes)
}
pub fn file(path: &Path) -> Result<Vec<u8>> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(2, "file name required"))?;
    read(parent, Path::new(name), false)
}
pub fn rows(bytes: &[u8]) -> Result<Vec<V>> {
    if oms_safe_fs::unfinished_jsonl(bytes) {
        return fail("unfinished JSONL record; reconcile the writer before retrying");
    }
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .enumerate()
        .map(|(i, line)| {
            if line.len() > MAX_ROW {
                return fail("JSONL row exceeds size limit");
            }
            let value = V::parse(line)
                .map_err(|_| Error::new(2, format!("invalid JSONL record {}", i + 1)))?;
            if !matches!(value, V::Obj(_)) {
                return fail("JSONL records must be objects");
            }
            Ok(value)
        })
        .collect()
}
pub fn load(repo: &Path, path: &Path) -> Result<Vec<V>> {
    rows(&read_jsonl(repo, path, true)?)
}
pub fn latest(rows: &[V]) -> Result<Vec<V>> {
    let mut positions = std::collections::HashMap::new();
    let mut values: Vec<&V> = Vec::new();
    for row in rows {
        let ident = crate::text(row, "id");
        if ident.is_empty() {
            return fail("record has no id");
        }
        if let Some(&position) = positions.get(ident) {
            values[position] = row;
        } else {
            positions.insert(ident, values.len());
            values.push(row);
        }
    }
    Ok(values.into_iter().cloned().collect())
}
pub fn lock(repo: &Path, path: &Path, interrupted: &AtomicBool) -> Result<oms_lock::Guard> {
    relative(path)?;
    let anchor = root(repo)?;
    let canonical = anchor.physical_root().join(path);
    let mut settings =
        oms_lock::Settings::from_env().map_err(|_| Error::new(2, "invalid lock settings"))?;
    settings.cwd = anchor.physical_root().to_owned();
    let timeout = settings.timeout;
    let lock = oms_lock::Lock::for_file(&canonical, settings)
        .map_err(|_| Error::new(2, "cannot derive state lock"))?;
    let start = Instant::now();
    loop {
        check(interrupted)?;
        match lock.try_acquire() {
            Ok(g) => return Ok(g),
            Err(oms_lock::Error::Busy) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => return Err(Error::new(e.exit_code(), "could not acquire state lock")),
        }
    }
}

pub fn transaction_locked<T>(
    repo: &Path,
    path: &Path,
    guard: &oms_lock::Guard,
    interrupted: &AtomicBool,
    f: impl FnOnce(&[u8]) -> Result<(Vec<u8>, T)>,
) -> Result<T> {
    relative(path)?;
    let anchor = root(repo)?;
    check(interrupted)?;
    let mut result = None;
    anchor
        .atomic_mutate(
            path,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: MAX_FILE,
                },
                create_parent: true,
                max_output: MAX_FILE,
                ..Default::default()
            },
            |old| {
                guard
                    .check()
                    .map_err(|_| Error::new(2, "state lock ownership changed"))?;
                let (bytes, value) = f(old)?;
                check(interrupted)?;
                result = Some(value);
                Ok::<_, Error>(bytes)
            },
        )
        .map_err(|e| {
            Error::new(
                2,
                format!("state publication failed; reconcile before retry: {e:?}"),
            )
        })?;
    guard
        .check()
        .map_err(|_| Error::new(2, "state lock ownership changed after publication"))?;
    result.ok_or_else(|| Error::new(2, "state transform did not run"))
}

pub fn transaction<T>(
    repo: &Path,
    path: &Path,
    interrupted: &AtomicBool,
    f: impl FnOnce(&[u8]) -> Result<(Vec<u8>, T)>,
) -> Result<T> {
    let guard = lock(repo, path, interrupted)?;
    transaction_locked(repo, path, &guard, interrupted, f)
}

pub fn write_locked(
    repo: &Path,
    path: &Path,
    guard: &oms_lock::Guard,
    bytes: Vec<u8>,
    replace: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    transaction_locked(repo, path, guard, interrupted, |_old| {
        if !replace && std::fs::symlink_metadata(repo.join(path)).is_ok() {
            return fail("destination already exists");
        }
        Ok((bytes, ()))
    })
}

pub fn write(
    repo: &Path,
    path: &Path,
    bytes: Vec<u8>,
    replace: bool,
    interrupted: &AtomicBool,
) -> Result<()> {
    let guard = lock(repo, path, interrupted)?;
    write_locked(repo, path, &guard, bytes, replace, interrupted)
}
pub fn append(repo: &Path, path: &Path, row: &V, interrupted: &AtomicBool) -> Result<()> {
    let mut body = row.json(false)?;
    body.push(b'\n');
    if body.len() > MAX_ROW {
        return fail("record exceeds size limit");
    }
    log(repo, path, interrupted, |old, _| {
        rows(old)?;
        Ok((body, ()))
    })
}

/// Retains the inode used by thread delivery cursors. A kill during a short
/// write can leave an incomplete tail; all later writes refuse that tail.
#[allow(clippy::unnecessary_cast)]
pub fn log<T>(
    repo: &Path,
    path: &Path,
    interrupted: &AtomicBool,
    f: impl FnOnce(&[u8], &std::fs::Metadata) -> Result<(Vec<u8>, T)>,
) -> Result<T> {
    use rustix::fs::{self as fs, FileType, Mode, OFlags};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    relative(path)?;
    let anchor = root(repo)?;
    let mut settings =
        oms_lock::Settings::from_env().map_err(|_| Error::new(2, "invalid lock settings"))?;
    settings.cwd = anchor.physical_root().to_owned();
    let timeout = settings.timeout;
    let lock = oms_lock::Lock::for_file(&anchor.physical_root().join(path), settings)
        .map_err(|_| Error::new(2, "cannot derive log lock"))?;
    let deadline = Instant::now();
    let guard = loop {
        check(interrupted)?;
        match lock.try_acquire() {
            Ok(guard) => break guard,
            Err(oms_lock::Error::Busy) if deadline.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(Error::new(error.exit_code(), "cannot acquire log lock")),
        }
    };
    check(interrupted)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = fs::open(anchor.physical_root(), flags, Mode::empty())
        .map_err(|_| Error::new(2, "log root unavailable"))?;
    if let Some(parent) = path.parent() {
        for part in parent.components() {
            let Component::Normal(part) = part else {
                return fail("invalid log directory");
            };
            match fs::mkdirat(&directory, part, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return fail("cannot create log directory"),
            }
            directory = fs::openat(&directory, part, flags, Mode::empty())
                .map_err(|_| Error::new(2, "unsafe log directory"))?;
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(2, "missing log name"))?;
    let mut file: std::fs::File = fs::openat(
        &directory,
        name,
        OFlags::RDWR
            | OFlags::APPEND
            | OFlags::CREATE
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| Error::new(2, "unsafe log file"))?
    .into();
    let before = file.metadata()?;
    if !before.is_file() || before.nlink() != 1 || before.len() > MAX_FILE as u64 {
        return fail("unsafe or oversized log file");
    }
    let mut old = Vec::new();
    (&mut file)
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut old)?;
    if old.len() > MAX_FILE || (!old.is_empty() && !old.ends_with(b"\n")) {
        return fail("log has an oversized or unfinished record; reconcile before retry");
    }
    let (bytes, value) = f(&old, &before)?;
    if old.len().saturating_add(bytes.len()) > MAX_FILE {
        return fail("log exceeds storage budget");
    }
    guard
        .check()
        .map_err(|_| Error::new(2, "log lock ownership changed"))?;
    let named = fs::statat(&directory, name, fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| Error::new(2, "log disappeared before append"))?;
    if FileType::from_raw_mode(named.st_mode) != FileType::RegularFile
        || named.st_dev as u64 != before.dev()
        || named.st_ino as u64 != before.ino()
        || named.st_nlink != 1
        || named.st_size as u64 != before.len()
        || file.seek(SeekFrom::End(0))? != before.len()
    {
        return fail("log identity or size changed before append");
    }
    check(interrupted)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::fsync(&directory).map_err(|_| {
        Error::new(
            2,
            "log published but directory durability uncertain; reconcile before retry",
        )
    })?;
    guard
        .check()
        .map_err(|_| Error::new(2, "log lock ownership changed after append"))?;
    Ok(value)
}
pub(crate) fn repository(context: &crate::Context<'_>, raw: &Path) -> Result<PathBuf> {
    let path = context
        .cwd
        .join(raw)
        .canonicalize()
        .map_err(|_| Error::new(2, "repository directory is unavailable"))?;
    let result = crate::process::git(context, &path, &["rev-parse", "--show-toplevel"]);
    match result {
        Ok(bytes) => {
            use std::os::unix::ffi::OsStrExt;
            let trimmed = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
            Path::new(std::ffi::OsStr::from_bytes(trimmed))
                .canonicalize()
                .map_err(Error::from)
        }
        Err(_) => Ok(path),
    }
}
pub fn names(repo: &Path, path: &Path, extension: &str) -> Result<Vec<String>> {
    relative(path)?;
    let anchor = root(repo)?;
    let mut current = anchor.physical_root().to_owned();
    for part in path.components() {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            _ => return fail("state directory must not be symlinked"),
        }
    }
    let mut values = Vec::new();
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        if values.len() >= 10_000 {
            return fail("directory exceeds scan entry limit");
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(extension) {
            values.push(name);
        }
    }
    values.sort();
    Ok(values)
}
