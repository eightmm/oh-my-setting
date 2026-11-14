use crate::store_path::{Anchor, regular, sync_directory};
use crate::value;
use crate::{Context, Error, Result};
use oms_context::Json;
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use rustix::fs::{self, Mode, OFlags};
use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

pub const ROW_BYTES: usize = 4 * 1024 * 1024;
/// In-memory report selection only; never a bound on untouched append history.
pub const LEDGER_BYTES: usize = 256 * 1024 * 1024;
pub fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
/// Comparison veto only: opening and authority checks always walk the original spelling.
pub fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::RootDir | std::path::Component::Normal(_) => out.push(c),
            std::path::Component::ParentDir => {
                out.pop();
            }
            _ => (),
        }
    }
    out
}
fn caller() -> Result<PathBuf> {
    std::env::current_dir().map_err(|_| Error::usage("caller root is unavailable"))
}
/// Retains every named ancestor and the leaf snapshot until completion is checked.
pub(crate) struct CheckedFile {
    file: File,
    anchor: Anchor,
    before: Metadata,
}
impl CheckedFile {
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }
    pub fn verify_unchanged(&self) -> Result<()> {
        self.anchor.verify_file(&self.file, &self.before)
    }
}
impl Read for CheckedFile {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.file.read(b)
    }
}
impl Seek for CheckedFile {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.file.seek(p)
    }
}
pub fn open(path: &Path, missing: bool) -> Result<Option<CheckedFile>> {
    open_in(path, missing, &caller()?)
}
pub(crate) fn open_in(path: &Path, missing: bool, root: &Path) -> Result<Option<CheckedFile>> {
    let Some(anchor) = Anchor::parent(path, root, false)? else {
        return if missing {
            Ok(None)
        } else {
            Err(Error::usage("record is Missing"))
        };
    };
    let Some(named) = anchor.leaf()? else {
        return if missing {
            Ok(None)
        } else {
            Err(Error::usage("record is Missing"))
        };
    };
    if !regular(&named) {
        return Err(Error::usage("record must be a regular single-link file"));
    }
    let fd = fs::openat(
        anchor.fd(),
        anchor.name(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )
    .map_err(|_| Error::usage("record changed or is unreadable"))?;
    let file = File::from(fd);
    let before = file.metadata()?;
    if before.dev() != named.st_dev as u64 || before.ino() != named.st_ino as u64 {
        return Err(Error::usage("record identity changed while opening"));
    }
    anchor.verify_file(&file, &before)?;
    Ok(Some(CheckedFile {
        file,
        anchor,
        before,
    }))
}
pub(crate) fn read_optional_in(path: &Path, max: usize, root: &Path) -> Result<Option<Vec<u8>>> {
    let Some(mut file) = open_in(path, true, root)? else {
        return Ok(None);
    };
    let mut bytes = vec![];
    let mut block = [0; 65536];
    loop {
        let remaining = max.saturating_sub(bytes.len());
        let count = block.len().min(remaining.saturating_add(1));
        let n = match file.read(&mut block[..count]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            break;
        }
        if n > remaining {
            return Err(Error::usage(
                "InputTooLarge: bounded metadata read exceeded its byte limit",
            ));
        }
        bytes.extend_from_slice(&block[..n]);
    }
    file.verify_unchanged()?;
    Ok(Some(bytes))
}
pub(crate) fn safe_read_in(path: &Path, max: usize, missing: bool, root: &Path) -> Result<Vec<u8>> {
    match read_optional_in(path, max, root)? {
        Some(bytes) => Ok(bytes),
        None if missing => Ok(vec![]),
        None => Err(Error::usage("record is Missing")),
    }
}
/// Each physical line is bounded; pathname replacement never validates a stale read.
#[cfg(test)]
pub fn lines(path: &Path, missing: bool, action: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    lines_in(path, missing, &caller()?, action)
}
pub(crate) fn lines_in(
    path: &Path,
    missing: bool,
    root: &Path,
    mut action: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let Some(file) = open_in(path, missing, root)? else {
        return Ok(());
    };
    let mut reader = BufReader::new(file);
    let mut row = vec![];
    loop {
        row.clear();
        let n = reader
            .by_ref()
            .take((ROW_BYTES + 1) as u64)
            .read_until(b'\n', &mut row)
            .map_err(|_| Error::io("reading ledger"))?;
        if n == 0 {
            break;
        }
        if row.len() > ROW_BYTES {
            return Err(Error::usage("ledger row exceeds 4194304 bytes"));
        }
        action(&row)?;
    }
    reader.get_ref().verify_unchanged().map_err(|e| {
        Error::usage(format!(
            "ledger changed during read or pathname proof failed: {}",
            e.message
        ))
    })
}
#[cfg(test)]
pub fn rows(path: &Path, missing: bool, action: impl FnMut(Json) -> Result<()>) -> Result<()> {
    rows_in(path, missing, &caller()?, action)
}
pub(crate) fn rows_in(
    path: &Path,
    missing: bool,
    root: &Path,
    mut action: impl FnMut(Json) -> Result<()>,
) -> Result<()> {
    lines_in(path, missing, root, |b| {
        if let Ok(v) = value::parse(b)
            && matches!(v, Json::Object(_))
        {
            action(v)?;
        }
        Ok(())
    })
}
pub fn lock(ctx: &Context<'_>, path: &Path) -> Result<oms_lock::Guard> {
    ctx.check()?;
    let mut settings = ctx.lock_settings.clone();
    settings.cwd = ctx.cwd.clone();
    let lock = oms_lock::Lock::for_file(path, settings).map_err(Error::lock)?;
    let started = std::time::Instant::now();
    loop {
        ctx.check()?;
        match lock.try_acquire() {
            Ok(g) => return Ok(g),
            Err(oms_lock::Error::Busy) => {
                if started.elapsed() >= ctx.lock_settings.timeout {
                    return Err(Error::lock(oms_lock::Error::Busy));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(Error::lock(e)),
        }
    }
}
fn writable(meta: &Metadata) -> Result<()> {
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o002 != 0
    {
        return Err(Error::usage(
            "record must be an owner-controlled, non-world-writable regular single-link file",
        ));
    }
    if meta.mode() & 0o222 == 0 {
        return Err(Error::usage("record has no write permission bits"));
    }
    Ok(())
}
fn append_uncertain(e: impl std::fmt::Display) -> Error {
    Error::io(format!(
        "append may be partial or durability uncertain; prior evidence was not rolled back; reconcile before retry: {e}"
    ))
}
fn append_locked(path: &Path, body: &[u8], guard: &oms_lock::Guard, root: &Path) -> Result<()> {
    let anchor =
        Anchor::parent(path, root, true)?.ok_or_else(|| Error::io("append parent is missing"))?;
    let named = anchor.leaf()?;
    if let Some(stat) = &named {
        if !regular(stat)
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_mode & 0o002 != 0
        {
            return Err(Error::usage(
                "append target must be owner-controlled, non-world-writable and single-link",
            ));
        }
        if stat.st_mode & 0o222 == 0 {
            return Err(Error::usage("record has no write permission bits"));
        }
    }
    guard.check().map_err(Error::lock)?;
    let mut flags = OFlags::WRONLY
        | OFlags::APPEND
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
        | OFlags::NOCTTY;
    if named.is_none() {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let fd = fs::openat(
        anchor.fd(),
        anchor.name(),
        flags,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| {
        Error::usage("append target changed or cannot be opened without following links")
    })?;
    let mut file = File::from(fd);
    let before = file.metadata()?;
    if let Some(named) = named {
        if before.dev() != named.st_dev as u64 || before.ino() != named.st_ino as u64 {
            return Err(Error::usage("append target identity changed"));
        }
        writable(&before)?;
    } else if !before.is_file()
        || before.nlink() != 1
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.mode() & 0o002 != 0
    {
        return Err(Error::usage("unsafe new append target"));
    }
    anchor.verify_file(&file, &before)?;
    let separator = if !body.is_empty() && before.len() > 0 {
        let tail_fd = fs::openat(
            anchor.fd(),
            anchor.name(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::empty(),
        )
        .map_err(|_| Error::usage("cannot open the existing final byte; append refused"))?;
        let tail = File::from(tail_fd);
        anchor.verify_file(&tail, &before)?;
        let mut byte = [0];
        tail.read_exact_at(&mut byte, before.len() - 1)
            .map_err(|_| {
                Error::usage("cannot establish the existing final byte; append refused")
            })?;
        byte[0] != b'\n'
    } else {
        false
    };
    let added = body
        .len()
        .checked_add(usize::from(separator))
        .ok_or_else(|| Error::usage("append length overflow"))?;
    let expected = before
        .len()
        .checked_add(added as u64)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| Error::usage("append exceeds the supported file-offset range"))?;
    let mut record = Vec::with_capacity(added);
    if separator {
        record.push(b'\n');
    }
    record.extend_from_slice(body);
    anchor.verify_file(&file, &before)?;
    guard.check().map_err(Error::lock)?;
    if let Err(e) = file.write_all(&record) {
        let _ = file.sync_all();
        let _ = sync_directory(anchor.fd());
        return Err(append_uncertain(e));
    }
    let finish = || -> Result<()> {
        file.sync_all()?;
        sync_directory(anchor.fd())?;
        guard.check().map_err(Error::lock)?;
        let after = file.metadata()?;
        writable_after_create(&after)?;
        if after.len() != expected {
            return Err(Error::usage("append length changed outside this operation"));
        }
        anchor.verify_file(&file, &after)
    };
    finish().map_err(append_uncertain)
}
fn writable_after_create(meta: &Metadata) -> Result<()> {
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o002 != 0
    {
        return Err(Error::usage("append write authority changed"));
    }
    Ok(())
}
pub fn write_locked(
    path: &Path,
    body: &[u8],
    append: bool,
    guard: &oms_lock::Guard,
    root: &Path,
) -> Result<()> {
    guard.check().map_err(Error::lock)?;
    if body.len() > ROW_BYTES {
        return Err(Error::usage("new record exceeds 4194304 bytes"));
    }
    if append {
        return append_locked(path, body, guard, root);
    }
    let anchor = Anchor::parent(path, root, true)?
        .ok_or_else(|| Error::io("replacement parent is missing"))?;
    let existing = open_in(path, true, root)?;
    if let Some(file) = &existing {
        writable(&file.before)?;
        file.verify_unchanged()?;
    }
    let parent = anchor.physical_parent(path)?;
    let safe = Root::open(&parent).map_err(|_| Error::io("opening replacement root"))?;
    let safe_named =
        fs::stat(safe.physical_root()).map_err(|_| Error::io("checking replacement root"))?;
    let retained = fs::fstat(anchor.fd()).map_err(|_| Error::io("checking replacement parent"))?;
    if safe_named.st_dev != retained.st_dev || safe_named.st_ino != retained.st_ino {
        return Err(Error::usage("replacement root identity changed"));
    }
    safe.atomic_mutate(
        Path::new(anchor.name()),
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: ROW_BYTES,
            },
            create_parent: false,
            max_output: ROW_BYTES,
            expected_exists: Some(existing.is_some()),
            ..MutationOptions::default()
        },
        |_| -> Result<Vec<u8>> {
            guard.check().map_err(Error::lock)?;
            anchor.verify()?;
            if let Some(file) = &existing {
                file.verify_unchanged()?;
            }
            Ok(body.to_vec())
        },
    )
    .map_err(|e| {
        Error::io(format!(
            "record publication failed; reconcile before retry: {e:?}"
        ))
    })?;
    anchor.verify().map_err(|e| {
        Error::io(format!(
            "replacement may be published but pathname proof failed; reconcile before retry: {}",
            e.message
        ))
    })?;
    Ok(())
}
pub fn append(ctx: &Context<'_>, path: &Path, row: &Json) -> Result<()> {
    let mut bytes = value::ordered(row)?;
    bytes.push(b'\n');
    let guard = lock(ctx, path)?;
    write_locked(path, &bytes, true, &guard, &ctx.cwd)?;
    guard.release().map_err(Error::lock)
}
/// Final execution audit is published after cancellation, once all children are reaped.
pub fn append_final(ctx: &Context<'_>, path: &Path, row: &Json) -> Result<()> {
    let running = std::sync::atomic::AtomicBool::new(false);
    let final_ctx = Context {
        cwd: ctx.cwd.clone(),
        env: ctx.env.clone(),
        interrupted: &running,
        signal: None,
        lock_settings: ctx.lock_settings.clone(),
        query_timeout: ctx.query_timeout,
        command_timeout: None,
    };
    append(&final_ctx, path, row)
}
