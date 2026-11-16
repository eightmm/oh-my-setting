use crate::{Error, Result, V, require};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) const MAX: usize = 256 * 1024 * 1024;
pub(crate) fn root(repo: &Path) -> Result<Root> {
    Root::open(repo).map_err(|_| Error::new(2, "unsafe storage root"))
}
pub(crate) fn read(repo: &Path, path: &Path, missing: bool) -> Result<Vec<u8>> {
    root(repo)?
        .read_no_follow(
            path,
            ByteReadOptions {
                missing_ok: missing,
                max_bytes: MAX,
            },
        )
        .map_err(|e| Error::new(2, format!("unsafe or unreadable state: {e:?}")))
}
pub(crate) fn read_external(path: &Path, missing: bool) -> Result<Vec<u8>> {
    require(path.is_absolute(), "state path must be absolute")?;
    let mut ancestor = path
        .parent()
        .ok_or_else(|| Error::new(2, "invalid state path"))?;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::new(2, "missing state root"))?;
    }
    let physical = std::fs::canonicalize(ancestor)?;
    let relative = path
        .strip_prefix(ancestor)
        .map_err(|_| Error::new(2, "invalid state path"))?;
    read(&physical, relative, missing)
}
pub(crate) fn lock(path: &Path, interrupted: &AtomicBool) -> Result<oms_lock::Guard> {
    crate::check(interrupted)?;
    let settings =
        oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let lock = oms_lock::Lock::for_file(path, settings)
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    crate::check(interrupted)?;
    match lock.try_acquire() {
        Ok(g) => return Ok(g),
        Err(oms_lock::Error::Busy) => {}
        Err(e) => return Err(Error::new(e.exit_code(), e.to_string())),
    }
    let cancel = oms_lock::Cancellation::default();
    let done = std::sync::atomic::AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if interrupted.load(std::sync::atomic::Ordering::Relaxed) {
                    cancel.cancel();
                    break;
                }
                std::thread::park_timeout(std::time::Duration::from_millis(20));
            }
        });
        let result = lock.acquire(Some(&cancel));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        result
    });
    result.map_err(|e| Error::new(e.exit_code(), e.to_string()))
}

pub(crate) fn replace(repo: &Path, path: &Path, old: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
    root(repo)?
        .atomic_mutate(
            path,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: old.is_none(),
                    max_bytes: MAX,
                },
                create_parent: true,
                max_output: MAX,
                expected_exists: Some(old.is_some()),
                ..MutationOptions::default()
            },
            |current| -> std::result::Result<Vec<u8>, String> {
                if current != old.unwrap_or_default() {
                    return Err("state compare-and-set failed".into());
                }
                Ok(bytes.to_vec())
            },
        )
        .map_err(|e| {
            Error::new(
                2,
                format!("state publication failed; reconcile before retry: {e:?}"),
            )
        })?;
    Ok(())
}
pub(crate) fn write_external(
    path: &Path,
    old: Option<&[u8]>,
    bytes: &[u8],
    interrupted: &AtomicBool,
) -> Result<()> {
    let _guard = lock(path, interrupted)?;
    let mut ancestor = path
        .parent()
        .ok_or_else(|| Error::new(2, "invalid state path"))?;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::new(2, "invalid state path"))?;
    }
    let physical = std::fs::canonicalize(ancestor)?;
    replace(
        &physical,
        path.strip_prefix(ancestor)
            .map_err(|_| Error::new(2, "invalid state path"))?,
        old,
        bytes,
    )
}
pub(crate) fn rows(repo: &Path, path: &Path) -> Result<Vec<V>> {
    let raw = read(repo, path, true)?;
    parse_rows(&raw)
}
pub(crate) fn parse_rows(raw: &[u8]) -> Result<Vec<V>> {
    require(
        raw.is_empty() || raw.ends_with(b"\n"),
        "incomplete JSONL tail; repair before appending",
    )?;
    raw.split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(|l| V::parse(l).map_err(Error::from))
        .collect()
}
pub(crate) fn append(repo: &Path, path: &Path, row: &V, interrupted: &AtomicBool) -> Result<()> {
    let path = if path.is_absolute() {
        path.strip_prefix(repo)
            .map_err(|_| Error::new(2, "ledger leaves repository"))?
    } else {
        path
    };
    let _guard = lock(&repo.join(path), interrupted)?;
    append_under_lock(repo, path, row)
}
/// The caller retains the canonical file lock across its entire transaction.
pub(crate) fn append_under_lock(repo: &Path, path: &Path, row: &V) -> Result<()> {
    let store = oms_command_plan::storage::Store::open(repo)?;
    if !repo.join(path).try_exists()? {
        replace(repo, path, None, b"")?;
    }
    let value = oms_command_plan::json::parse(&row.json(false)?)?;
    store.append(path, &value)?;
    Ok(())
}

pub(crate) fn ensure_ignore(repo: &Path, interrupted: &AtomicBool) -> Result<()> {
    let path = Path::new(".oms/.gitignore");
    let _guard = lock(&repo.join(path), interrupted)?;
    if !repo.join(path).try_exists()? {
        replace(repo, path, None, b"*\n")?;
    } else {
        read(repo, path, false)?;
    }
    Ok(())
}
pub(crate) struct Temp {
    pub path: PathBuf,
}

/// O_EXCL prevents an absence-check/rename race from replacing newer work.
pub(crate) fn create_exclusive(
    repo: &Path,
    path: &Path,
    body: &[u8],
    mode: u32,
    parents: bool,
) -> Result<std::fs::File> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use std::io::Write;
    let relative = if path.is_absolute() {
        path.strip_prefix(repo)
            .map_err(|_| Error::new(2, "output leaves its storage root"))?
    } else {
        path
    };
    let parts: Vec<_> = relative
        .components()
        .map(|c| match c {
            std::path::Component::Normal(p) => Ok(p),
            _ => Err(Error::new(2, "unsafe output path")),
        })
        .collect::<Result<_>>()?;
    require(!parts.is_empty(), "output has no filename")?;
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut descriptors = vec![
        fs::open(repo, directory, Mode::empty())
            .map_err(|_| Error::new(2, "unsafe output root"))?,
    ];
    let mut generations =
        vec![fs::fstat(&descriptors[0]).map_err(|_| Error::new(2, "output root unavailable"))?];
    for part in &parts[..parts.len() - 1] {
        let fd = descriptors.last().unwrap();
        if parents {
            match fs::mkdirat(fd, *part, Mode::from_raw_mode(0o700)) {
                Ok(()) => fs::fsync(fd)
                    .map_err(|_| Error::new(2, "output directory durability uncertain"))?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return Err(Error::new(2, "cannot create output directory")),
            }
        }
        let next = fs::openat(fd, *part, directory, Mode::empty())
            .map_err(|_| Error::new(2, "unsafe output ancestry"))?;
        generations
            .push(fs::fstat(&next).map_err(|_| Error::new(2, "output ancestry unavailable"))?);
        descriptors.push(next);
    }
    let parent = descriptors.last().unwrap();
    #[cfg(target_os = "macos")]
    let raw_mode = mode as u16;
    #[cfg(not(target_os = "macos"))]
    let raw_mode = mode;
    let fd = fs::openat(
        parent,
        parts[parts.len() - 1],
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(raw_mode),
    )
    .map_err(|_| Error::new(2, "destination exists or cannot be exclusively created"))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(body)?;
    file.sync_all()?;
    fs::fsync(parent)
        .map_err(|_| Error::new(2, "output is visible; directory durability uncertain"))?;
    let same = |a: &rustix::fs::Stat, b: &rustix::fs::Stat| {
        a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && FileType::from_raw_mode(a.st_mode) == FileType::from_raw_mode(b.st_mode)
    };
    let named = fs::stat(repo).map_err(|_| Error::new(2, "output root changed"))?;
    require(
        same(&named, &generations[0]),
        "output root changed; reconcile before retry",
    )?;
    for i in 0..descriptors.len() - 1 {
        let named = fs::statat(&descriptors[i], parts[i], AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::new(2, "output ancestry changed"))?;
        require(
            same(&named, &generations[i + 1]),
            "output ancestry changed; reconcile before retry",
        )?;
    }
    Ok(file)
}
impl Temp {
    pub fn new(prefix: &str) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let parent = std::fs::canonicalize(std::env::temp_dir())?;
        for _ in 0..16 {
            let path = parent.join(format!(
                "{prefix}.{}",
                oms_command_plan::storage::random_hex(16)?
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::new(2, "cannot allocate private temporary directory"))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
