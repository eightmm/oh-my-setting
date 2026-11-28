use crate::{Result, Value, error, json};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use std::path::Path;

pub const MAX_FILE: usize = 8 * 1024 * 1024;
pub const MAX_STREAM: usize = 32 * 1024 * 1024;

pub fn read(root: &Path, path: &Path, limit: usize, missing: bool) -> Result<Vec<u8>> {
    let root = anchor(root, path);
    Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .read_no_follow(
            path,
            ByteReadOptions {
                missing_ok: missing,
                max_bytes: limit,
            },
        )
        .map_err(|e| error(2, format!("state read: {e:?}")))
}

pub fn read_retrying(root: &Path, path: &Path, limit: usize, missing: bool) -> Result<Vec<u8>> {
    let root = anchor(root, path);
    Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .read_no_follow_retrying(
            path,
            ByteReadOptions {
                missing_ok: missing,
                max_bytes: limit,
            },
        )
        .map_err(|e| error(2, format!("state read: {e:?}")))
}

pub fn lock(path: &Path) -> Result<oms_lock::Guard> {
    let settings = oms_lock::Settings::from_env().map_err(|e| error(e.exit_code(), e))?;
    oms_lock::Lock::for_file(path, settings)
        .and_then(|l| l.acquire(None))
        .map_err(|e| error(e.exit_code(), e))
}

pub fn try_lock(path: &Path) -> Result<Option<oms_lock::Guard>> {
    let settings = oms_lock::Settings::from_env().map_err(|e| error(e.exit_code(), e))?;
    try_lock_with(path, settings)
}

pub fn try_lock_in(cwd: &Path, path: &Path) -> Result<Option<oms_lock::Guard>> {
    let mut settings = oms_lock::Settings::from_env().map_err(|e| error(e.exit_code(), e))?;
    settings.cwd = cwd.to_owned();
    try_lock_with(path, settings)
}

fn try_lock_with(path: &Path, settings: oms_lock::Settings) -> Result<Option<oms_lock::Guard>> {
    match oms_lock::Lock::for_file(path, settings).and_then(|l| l.try_acquire()) {
        Ok(guard) => Ok(Some(guard)),
        Err(oms_lock::Error::Busy) => Ok(None),
        Err(e) => Err(error(e.exit_code(), e)),
    }
}

pub fn replace_locked(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let root = anchor(root, path);
    Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .atomic_mutate(
            path,
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: MAX_STREAM,
                },
                create_parent: true,
                max_output: MAX_STREAM,
                ..Default::default()
            },
            |_| Ok::<_, String>(bytes.to_vec()),
        )
        .map_err(|e| {
            error(
                1,
                format!("state publication: {e:?}; reconcile before retry"),
            )
        })?;
    Ok(())
}

pub fn rows(root: &Path, path: &Path) -> Result<(Vec<Value>, usize)> {
    let bytes = read(root, path, MAX_STREAM, true)?;
    Ok(parse_rows(&bytes))
}

pub fn thread_rows(root: &Path, path: &Path) -> Result<(Vec<Value>, usize)> {
    let root = anchor(root, path);
    let bytes = Root::open(root)
        .map_err(|e| error(2, format!("{e:?}")))?
        .read_no_follow_jsonl(
            path,
            ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_STREAM,
            },
        )
        .map_err(|e| error(2, format!("state read: {e:?}")))?;
    Ok(parse_rows(&bytes))
}

pub fn parse_rows(bytes: &[u8]) -> (Vec<Value>, usize) {
    let mut rows = Vec::new();
    let mut bad = 0;
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
    {
        match json::parse_legacy(line, MAX_FILE) {
            Ok(value) if matches!(value, Value::Object(_)) => rows.push(value),
            _ => bad += 1,
        }
    }
    (rows, bad)
}

/// The caller holds the canonical stream lock, including newline recovery.
pub fn append_locked(root: &Path, path: &Path, row: &Value) -> Result<()> {
    append_many_locked(root, path, std::slice::from_ref(row))
}

pub fn append_many_locked(root: &Path, path: &Path, rows: &[Value]) -> Result<()> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    if rows.is_empty() {
        return Ok(());
    }
    let root = anchor(root, path);
    let relative = if path.is_absolute() {
        path.strip_prefix(root).map_err(|e| error(2, e))?
    } else {
        path
    };
    let parts = relative
        .components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(error(2, "invalid append path")),
        })
        .collect::<Result<Vec<_>>>()?;
    crate::require(!parts.is_empty(), "append path has no filename")?;
    let physical = std::fs::canonicalize(root).map_err(|e| error(2, e))?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dirs = vec![fs::open(&physical, flags, Mode::empty()).map_err(|e| error(2, e))?];
    for name in &parts[..parts.len() - 1] {
        let parent = dirs.last().expect("root descriptor");
        match fs::mkdirat(parent, *name, Mode::from_raw_mode(0o700)) {
            Ok(()) => sync_directory(parent)?,
            Err(rustix::io::Errno::EXIST) => (),
            Err(e) => return Err(error(2, e)),
        }
        dirs.push(fs::openat(parent, *name, flags, Mode::empty()).map_err(|e| error(2, e))?);
    }
    let parent = dirs.last().expect("root descriptor");
    let name = parts[parts.len() - 1];
    let fd = fs::openat(
        parent,
        name,
        OFlags::WRONLY
            | OFlags::CREATE
            | OFlags::APPEND
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| error(2, e))?;
    let mut output = std::fs::File::from(fd);
    let opened = output.metadata().map_err(|e| error(2, e))?;
    let check = || -> Result<()> {
        let root_stat =
            fs::statat(fs::CWD, &physical, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| error(2, e))?;
        let identity = |a: &fs::Stat, b: &fs::Stat| a.st_dev == b.st_dev && a.st_ino == b.st_ino;
        crate::require(
            identity(&root_stat, &fs::fstat(&dirs[0]).map_err(|e| error(2, e))?),
            "append root changed",
        )?;
        for (i, name) in parts[..parts.len() - 1].iter().enumerate() {
            let named =
                fs::statat(&dirs[i], *name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| error(2, e))?;
            crate::require(
                identity(&named, &fs::fstat(&dirs[i + 1]).map_err(|e| error(2, e))?),
                "append directory changed",
            )?;
        }
        let named = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| error(2, e))?;
        crate::require(
            FileType::from_raw_mode(named.st_mode) == FileType::RegularFile
                && named.st_nlink == 1
                && named.st_uid == rustix::process::geteuid().as_raw()
                && named.st_dev as u64 == opened.dev()
                && named.st_ino as u64 == opened.ino(),
            "append target is linked, changed or not an owned regular file",
        )
    };
    check()?;
    crate::require(
        opened.len() <= MAX_STREAM as u64,
        "append stream exceeds byte limit",
    )?;
    let mut bytes = Vec::new();
    if opened.len() > 0 {
        let input = fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| error(2, e))?;
        let mut input = std::fs::File::from(input);
        let metadata = input.metadata().map_err(|e| error(2, e))?;
        crate::require(
            metadata.dev() == opened.dev() && metadata.ino() == opened.ino(),
            "append input changed",
        )?;
        input.seek(SeekFrom::End(-1)).map_err(|e| error(2, e))?;
        let mut last = [0];
        input.read_exact(&mut last).map_err(|e| error(2, e))?;
        if last[0] != b'\n' {
            bytes.push(b'\n');
        }
    }
    for row in rows {
        let line = json::default_bytes(row, false)?;
        crate::require(line.len() <= MAX_FILE, "append row exceeds byte limit")?;
        bytes.extend_from_slice(&line);
        bytes.push(b'\n');
        crate::require(
            bytes.len() <= MAX_STREAM.saturating_sub(opened.len() as usize),
            "append stream exceeds byte limit",
        )?;
    }
    check()?;
    output
        .write_all(&bytes)
        .and_then(|_| output.sync_data())
        .map_err(|e| error(1, format!("state append: {e}; reconcile before retry")))?;
    check()?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(fd: &impl std::os::fd::AsFd) -> Result<()> {
    match rustix::fs::fsync(fd) {
        Ok(())
        | Err(rustix::io::Errno::BADF | rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => {
            Ok(())
        }
        Err(e) => Err(error(1, e)),
    }
}

// Absolute override paths are explicit destinations. Keep the descriptor walk
// no-follow, while choosing a root that includes those destinations.
fn anchor<'a>(root: &'a Path, path: &Path) -> &'a Path {
    if path.is_absolute() && !path.starts_with(root) {
        Path::new("/")
    } else {
        root
    }
}

pub fn input_file(path: &Path) -> Result<std::fs::File> {
    open_data(path, rustix::fs::OFlags::RDONLY)
}
pub fn log_file(path: &Path) -> Result<std::fs::File> {
    open_data(path, rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::TRUNC)
}
fn open_data(path: &Path, access: rustix::fs::OFlags) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::path::Component;
    require_absolute(path)?;
    let mut directory = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| error(2, e))?;
    let parts = path
        .components()
        .filter_map(|c| {
            if let Component::Normal(v) = c {
                Some(v)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        directory = openat(
            &directory,
            *part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| error(2, e))?;
    }
    let name = parts
        .last()
        .ok_or_else(|| error(2, "input has no filename"))?;
    let fd = openat(
        &directory,
        *name,
        access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| error(2, e))?;
    let file = std::fs::File::from(fd);
    crate::require(
        file.metadata().map_err(|e| error(2, e))?.is_file(),
        "input is not a regular file",
    )?;
    Ok(file)
}

fn require_absolute(path: &Path) -> Result<()> {
    crate::require(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "input path must be absolute without parent traversal",
    )
}

pub fn input(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut file = input_file(path)?;
    let before = file.metadata().map_err(|e| error(2, e))?;
    crate::require(before.len() <= limit as u64, "input exceeds byte limit")?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| error(2, e))?;
    crate::require(
        bytes.len() <= limit && file.metadata().map_err(|e| error(2, e))?.len() == before.len(),
        "input changed or exceeds byte limit",
    )?;
    Ok(bytes)
}

pub fn hash_input(path: &Path, limit: u64) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = input_file(path)?;
    let before = file.metadata().map_err(|e| error(2, e))?;
    crate::require(before.len() <= limit, "input exceeds hash limit")?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| error(2, e))?;
        if n == 0 {
            break;
        }
        count += n as u64;
        crate::require(count <= limit, "input exceeds hash limit")?;
        digest.update(&buffer[..n]);
    }
    let after = file.metadata().map_err(|e| error(2, e))?;
    crate::require(
        count == before.len()
            && before.modified().ok() == after.modified().ok()
            && before.len() == after.len(),
        "input changed while hashing",
    )?;
    Ok(format!("{:x}", digest.finalize()))
}

pub fn append(root: &Path, path: &Path, row: &Value) -> Result<()> {
    let target = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let _guard = lock(&target)?;
    append_locked(root, path, row)
}

/// Remove a retained-generation candidate using no-follow directory descriptors.
pub fn remove(root: &Path, path: &Path, expected: &std::fs::Metadata) -> Result<()> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};
    use std::os::unix::fs::MetadataExt;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| error(2, "GC target escaped root"))?;
    crate::helpers::relative(
        relative
            .to_str()
            .ok_or_else(|| error(2, "invalid GC path"))?,
    )?;
    let mut parent = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|e| error(2, e))?;
    let parts = relative.components().collect::<Vec<_>>();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        parent = openat(
            &parent,
            part.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|e| error(2, e))?;
    }
    let name = parts
        .last()
        .ok_or_else(|| error(2, "invalid GC target"))?
        .as_os_str();
    let current = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| error(2, e))?;
    crate::require(
        current.st_dev as u64 == expected.dev()
            && current.st_ino as u64 == expected.ino()
            && current.st_size as u64 == expected.len()
            && current.st_mtime as i64 == expected.mtime(),
        "GC target changed; retained",
    )?;
    fn tree(
        parent: &std::os::fd::OwnedFd,
        name: &std::ffi::CStr,
        budget: &mut usize,
    ) -> Result<()> {
        let before = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| error(2, e))?;
        if FileType::from_raw_mode(before.st_mode) == FileType::Directory {
            let directory = openat(
                parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| error(2, e))?;
            let opened = fstat(&directory).map_err(|e| error(2, e))?;
            crate::require(
                before.st_dev == opened.st_dev && before.st_ino == opened.st_ino,
                "GC directory changed",
            )?;
            for entry in Dir::read_from(&directory).map_err(|e| error(2, e))? {
                let entry = entry.map_err(|e| error(2, e))?;
                let child = entry.file_name();
                if [b".".as_slice(), b"..".as_slice()].contains(&child.to_bytes()) {
                    continue;
                }
                *budget = budget
                    .checked_sub(1)
                    .ok_or_else(|| error(2, "GC directory entry limit exceeded"))?;
                tree(&directory, child, budget)?;
            }
            unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|e| error(2, e))?;
        } else {
            crate::require(
                FileType::from_raw_mode(before.st_mode) == FileType::RegularFile,
                "GC refuses unknown or symlinked evidence",
            )?;
            unlinkat(parent, name, AtFlags::empty()).map_err(|e| error(2, e))?;
        }
        Ok(())
    }
    use std::os::unix::ffi::OsStrExt;
    let name =
        std::ffi::CString::new(name.as_bytes()).map_err(|_| error(2, "invalid GC filename"))?;
    tree(&parent, &name, &mut 50000)
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    #[test]
    fn append_limits_empty_batches_and_unsafe_targets() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let path = std::env::temp_dir().join(crate::random_id("oms-state-storage-").unwrap());
        std::fs::create_dir(&path).unwrap();
        let result = std::panic::catch_unwind(|| {
            let target = path.join("rows.jsonl");
            append_many_locked(&path, &target, &[]).unwrap();
            assert!(!target.exists());
            let empty = crate::json::empty();
            append_locked(&path, &target, &empty).unwrap();
            let inode = std::fs::metadata(&target).unwrap().ino();
            append_many_locked(&path, &target, &[empty.clone(), empty.clone()]).unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"{}\n{}\n{}\n");
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
            assert!(append_locked(&path, Path::new("../escape"), &empty).is_err());
            let linked = path.join("linked");
            symlink(&target, &linked).unwrap();
            assert!(append_locked(&path, &linked, &empty).is_err());
            let hard = path.join("hard");
            std::fs::hard_link(&target, &hard).unwrap();
            assert!(append_locked(&path, &hard, &empty).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"{}\n{}\n{}\n");
            let bounded = path.join("bounded");
            std::fs::write(&bounded, []).unwrap();
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&bounded)
                .unwrap();
            file.set_len((MAX_STREAM - 4) as u64).unwrap();
            append_locked(&path, &bounded, &empty).unwrap();
            assert_eq!(file.metadata().unwrap().len(), MAX_STREAM as u64);
            assert!(append_locked(&path, &bounded, &empty).is_err());
            assert_eq!(file.metadata().unwrap().len(), MAX_STREAM as u64);
            file.set_len(MAX_STREAM as u64 + 1).unwrap();
            assert!(append_locked(&path, &bounded, &empty).is_err());
            let row_path = path.join("large-row");
            let exact = crate::json::string("x".repeat(MAX_FILE - 2));
            append_locked(&path, &row_path, &exact).unwrap();
            let old = std::fs::metadata(&row_path).unwrap().len();
            let oversized = crate::json::string("x".repeat(MAX_FILE - 1));
            assert!(append_locked(&path, &row_path, &oversized).is_err());
            assert_eq!(std::fs::metadata(&row_path).unwrap().len(), old);
        });
        std::fs::remove_dir_all(path).unwrap();
        result.unwrap();
    }
}
