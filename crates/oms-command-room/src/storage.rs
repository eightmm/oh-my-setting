//! Descriptor-relative append storage. Locks serialize authority; snapshots never grant it.
use crate::value::{Value, parse};
use crate::{Error, Result, checkpoint};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAX_FILE: usize = 16 * 1024 * 1024;
pub const MAX_ROW: usize = 65536;
fn io(e: impl std::fmt::Display) -> Error {
    Error::new(format!("room storage: {e}"))
}
pub fn same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
pub fn stable(a: &Stat, b: &Stat) -> bool {
    same(a, b)
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_nlink == b.st_nlink
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn regular(s: &Stat) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile
        && s.st_nlink == 1
        && s.st_uid == rustix::process::geteuid().as_raw()
        && s.st_mode & 0o022 == 0
}
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
pub fn sync_dir(fd: &OwnedFd) -> Result<()> {
    match fs::fsync(fd) {
        Ok(()) | Err(Errno::BADF | Errno::INVAL | Errno::NOTSUP) => Ok(()),
        Err(e) => Err(io(e)),
    }
}
pub struct Directory {
    pub(crate) fd: OwnedFd,
    path: PathBuf,
    initial: Stat,
    protected: bool,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        let before = fs::statat(fs::CWD, path, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(Error::new("room directory must not be a link"));
        }
        let fd = fs::open(path, dir_flags(), Mode::empty()).map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        if !same(&before, &initial) {
            return Err(Error::new("room directory changed"));
        }
        Ok(Self {
            fd,
            path: path.to_owned(),
            initial,
            protected: false,
        })
    }
    pub fn private(&self) -> Result<()> {
        let s = fs::fstat(&self.fd).map_err(io)?;
        if s.st_uid != rustix::process::geteuid().as_raw() || s.st_mode & 0o077 != 0 {
            return Err(Error::new("room relay directory must be private and owned"));
        }
        Ok(())
    }
    pub fn check(&self) -> Result<()> {
        let named = fs::statat(fs::CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        let current = fs::fstat(&self.fd).map_err(io)?;
        if !same(&self.initial, &named)
            || !same(&self.initial, &current)
            || FileType::from_raw_mode(named.st_mode) != FileType::Directory
        {
            return Err(Error::new("room directory generation changed"));
        }
        if self.protected && (current.st_uid != self.initial.st_uid || current.st_mode & 0o022 != 0)
        {
            return Err(Error::new(
                "room directory ownership or permissions changed",
            ));
        }
        Ok(())
    }
    pub fn optional_child(&self, name: &OsStr) -> Result<Option<Self>> {
        self.check()?;
        match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => self.child(name, false).map(Some),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(io(e)),
        }
    }
    pub fn child(&self, name: &OsStr, create: bool) -> Result<Self> {
        self.check()?;
        if create {
            match fs::mkdirat(&self.fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => sync_dir(&self.fd)?,
                Err(Errno::EXIST) => (),
                Err(e) => return Err(io(e)),
            }
        }
        let before = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory
            || before.st_uid != rustix::process::geteuid().as_raw()
            || before.st_mode & 0o022 != 0
        {
            return Err(Error::new("unsafe room directory"));
        }
        let fd = fs::openat(&self.fd, name, dir_flags(), Mode::empty()).map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        if !same(&before, &initial) {
            return Err(Error::new("room directory changed"));
        }
        self.check()?;
        Ok(Self {
            fd,
            path: self.path.join(name),
            initial,
            protected: true,
        })
    }
    pub fn names(&self, cap: usize) -> Result<(Vec<OsString>, bool)> {
        let mut out = Vec::new();
        for e in fs::Dir::read_from(&self.fd).map_err(io)? {
            let e = e.map_err(io)?;
            let b = e.file_name().to_bytes();
            if b == b"." || b == b".." {
                continue;
            }
            if out.len() >= cap {
                return Ok((out, true));
            }
            out.push(OsString::from_vec(b.to_vec()));
        }
        Ok((out, false))
    }
    pub fn read(&self, name: &OsStr, cap: usize) -> Result<(Vec<u8>, Stat)> {
        let before = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !regular(&before) || before.st_size < 0 || before.st_size as u64 > cap as u64 {
            return Err(Error::new(
                "room input is oversized, linked or not a regular file",
            ));
        }
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let opened = fs::fstat(&fd).map_err(io)?;
        if !stable(&before, &opened) {
            return Err(Error::new("room input changed during open"));
        }
        let mut file = File::from(fd);
        let mut bytes = Vec::with_capacity(opened.st_size as usize);
        (&mut file)
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        let after = fs::fstat(&file).map_err(io)?;
        let named = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !stable(&opened, &after)
            || !stable(&opened, &named)
            || bytes.len() != opened.st_size as usize
        {
            return Err(Error::new("room input changed during read"));
        }
        Ok((bytes, opened))
    }
    pub fn prefix_tail(
        &self,
        name: &OsStr,
        prefix: usize,
        tail: usize,
    ) -> Result<(Vec<u8>, Vec<u8>, Stat)> {
        let before = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !regular(&before) || before.st_size < 0 || before.st_size as u64 > MAX_FILE as u64 {
            return Err(Error::new("unsafe or oversized room log"));
        }
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let opened = fs::fstat(&fd).map_err(io)?;
        if !stable(&before, &opened) {
            return Err(Error::new("room log changed"));
        }
        let mut f = File::from(fd);
        let mut first = Vec::new();
        if prefix > 0 {
            let mut reader = BufReader::new(&mut f);
            for _ in 0..2 {
                let start = first.len();
                (&mut reader)
                    .take((MAX_ROW + 1).min(prefix.saturating_sub(start) + 1) as u64)
                    .read_until(b'\n', &mut first)
                    .map_err(io)?;
                let line = &first[start..];
                if first.len() > prefix {
                    return Err(Error::new("room discovery byte limit"));
                }
                if line.len() > MAX_ROW || !line.is_empty() && !line.ends_with(b"\n") {
                    return Err(Error::new("room header is oversized or incomplete"));
                }
            }
        }
        let off = (opened.st_size as usize).saturating_sub(tail);
        f.seek(SeekFrom::Start(off as u64)).map_err(io)?;
        let mut last = vec![0; (opened.st_size as usize) - off];
        f.read_exact(&mut last).map_err(io)?;
        if !stable(&opened, &fs::fstat(&f).map_err(io)?)
            || !stable(
                &opened,
                &fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?,
            )
        {
            return Err(Error::new("room log changed"));
        }
        Ok((first, last, opened))
    }
    pub fn exists(&self, name: &OsStr) -> Result<bool> {
        match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) if regular(&s) => Ok(true),
            Ok(_) => Err(Error::new("unsafe room entry")),
            Err(Errno::NOENT) => Ok(false),
            Err(e) => Err(io(e)),
        }
    }
    pub fn create(&self, name: &OsStr, body: &[u8]) -> Result<()> {
        self.check()?;
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        if !regular(&initial) {
            return Err(Error::new("unsafe created room entry"));
        }
        if rustix::io::write(&fd, body).map_err(io)? != body.len() {
            return Err(Error::new(
                "room creation was short; inspect incomplete evidence",
            ));
        }
        fs::fsync(&fd).map_err(io)?;
        if !same(
            &initial,
            &fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?,
        ) {
            return Err(Error::new("room entry changed during creation"));
        }
        self.check()?;
        sync_dir(&self.fd)
    }
    pub fn append(&self, name: &OsStr, old: &Stat, body: &[u8]) -> Result<Stat> {
        self.check()?;
        let before = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !regular(&before) || !stable(old, &before) {
            return Err(Error::new("room log generation changed before append"));
        }
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let opened = fs::fstat(&fd).map_err(io)?;
        if !stable(old, &opened) {
            return Err(Error::new("room log changed before append"));
        }
        let mut file = File::from(fd);
        file.write_all(body).map_err(io)?;
        file.sync_data()
            .map_err(|e| Error::new(format!("room append durability unknown: {e}")))?;
        let fd = file;
        let after = fs::fstat(&fd).map_err(io)?;
        let named = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !regular(&after)
            || !stable(&after, &named)
            || !same(old, &after)
            || after.st_size != old.st_size + body.len() as i64
        {
            return Err(Error::new(
                "room append publication is uncertain; inspect evidence",
            ));
        }
        self.check()?;
        sync_dir(&self.fd)?;
        Ok(after)
    }
    /// Only the verified inode is removed. The directory remains descriptor-pinned.
    pub fn unlink(&self, name: &OsStr, expected: Option<&Stat>) -> Result<()> {
        if let Some(before) = expected {
            let now = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
            if !stable(before, &now) {
                return Err(Error::new("room entry changed before cleanup"));
            }
        }
        fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(io)?;
        sync_dir(&self.fd)
    }
    pub fn publish_queue(&self, temp: &OsStr, name: &OsStr, body: &[u8]) -> Result<()> {
        // Queue paths are intentionally never reopened: a renamed worker directory stays pinned.
        let fd = fs::openat(
            &self.fd,
            temp,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        let result = (|| {
            if rustix::io::write(&fd, body).map_err(io)? != body.len() {
                return Err(Error::new("short relay publication"));
            }
            fs::fsync(&fd).map_err(io)?;
            let named = fs::statat(&self.fd, temp, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
            if !same(&initial, &named) || !regular(&named) {
                return Err(Error::new("relay temporary file changed"));
            }
            fs::renameat(&self.fd, temp, &self.fd, name).map_err(io)?;
            sync_dir(&self.fd)
        })();
        if result.is_err()
            && let Ok(s) = fs::statat(&self.fd, temp, AtFlags::SYMLINK_NOFOLLOW)
            && same(&initial, &s)
            && regular(&s)
        {
            let _ = self.unlink(temp, Some(&s));
        }
        result
    }
}
/// A read-only poll descriptor. Unchanged ticks inspect metadata without reading bytes.
pub(crate) struct LogReader<'a> {
    directory: &'a Directory,
    name: OsString,
    file: File,
    #[cfg(test)]
    bytes_read: usize,
}
impl<'a> LogReader<'a> {
    pub(crate) fn open(directory: &'a Directory, name: OsString) -> Result<Self> {
        let fd = fs::openat(
            &directory.fd,
            &name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let reader = Self {
            directory,
            name,
            file: File::from(fd),
            #[cfg(test)]
            bytes_read: 0,
        };
        reader.stat()?;
        Ok(reader)
    }
    fn stat(&self) -> Result<Stat> {
        self.directory.check()?;
        let opened = fs::fstat(&self.file).map_err(io)?;
        let named =
            fs::statat(&self.directory.fd, &self.name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !regular(&named) || named.st_size < 0 || named.st_size as u64 > MAX_FILE as u64 {
            return Err(Error::new("unsafe or oversized room log"));
        }
        if !same(&opened, &named) {
            return Ok(named);
        }
        if !regular(&opened) || !stable(&opened, &named) {
            return Err(Error::new("room input changed during stat"));
        }
        Ok(opened)
    }
    pub(crate) fn poll(&mut self, old: &Snapshot) -> Result<Option<Snapshot>> {
        let stat = self.stat()?;
        if same(&old.stat, &stat) && old.stat.st_size == stat.st_size {
            return Ok(None);
        }
        self.refresh(old)
    }
    pub(crate) fn refresh(&mut self, old: &Snapshot) -> Result<Option<Snapshot>> {
        let stat = self.stat()?;
        if stable(&old.stat, &stat) {
            return Ok(None);
        }
        let opened = fs::fstat(&self.file).map_err(io)?;
        if !same(&opened, &stat) {
            *self = Self::open(self.directory, self.name.clone())?;
        }
        let mut incremental = same(&old.stat, &stat) && stat.st_size as usize >= old.bytes.len();
        if incremental {
            let length = old.bytes.len().min(256);
            let mut prefix = vec![0; length];
            self.file.seek(SeekFrom::Start(0)).map_err(io)?;
            self.file.read_exact(&mut prefix).map_err(io)?;
            #[cfg(test)]
            {
                self.bytes_read += prefix.len();
            }
            incremental = crate::digest(&prefix) == crate::digest(&old.bytes[..length]);
            // Preserve cursor boundary checks when an in-place edit accompanies growth.
            let start = old.bytes.len().saturating_sub(256);
            let mut boundary = vec![0; old.bytes.len() - start];
            self.file.seek(SeekFrom::Start(start as u64)).map_err(io)?;
            self.file.read_exact(&mut boundary).map_err(io)?;
            #[cfg(test)]
            {
                self.bytes_read += boundary.len();
            }
            incremental &= boundary == old.bytes[start..];
        }
        let offset = if incremental { old.bytes.len() } else { 0 };
        self.file.seek(SeekFrom::Start(offset as u64)).map_err(io)?;
        let mut bytes = vec![0; stat.st_size as usize - offset];
        self.file.read_exact(&mut bytes).map_err(io)?;
        #[cfg(test)]
        {
            self.bytes_read += bytes.len();
        }
        if !stable(&stat, &self.stat()?) || !stable(&stat, &fs::fstat(&self.file).map_err(io)?) {
            return Err(Error::new("room input changed during read"));
        }
        let mut snapshot = Snapshot::decode(&old.id, bytes, stat)?;
        if incremental {
            for range in &mut snapshot.ranges {
                range.start += offset;
                range.end += offset;
            }
            let mut bytes = old.bytes.clone();
            bytes.extend_from_slice(&snapshot.bytes);
            snapshot.bytes = bytes;
            let mut rows = old.rows.clone();
            rows.append(&mut snapshot.rows);
            snapshot.rows = rows;
            let mut ranges = old.ranges.clone();
            ranges.append(&mut snapshot.ranges);
            snapshot.ranges = ranges;
        }
        Ok(Some(snapshot))
    }
}

pub struct Store {
    pub root: Directory,
    pub oms: Directory,
    pub threads: Directory,
    pub repo: PathBuf,
}
impl Store {
    pub fn open(repo: &Path, create: bool) -> Result<Self> {
        let repo = std::fs::canonicalize(repo).map_err(io)?;
        let root = Directory::open(&repo)?;
        let oms = root.child(OsStr::new(".oms"), create)?;
        let threads = oms.child(OsStr::new("threads"), create)?;
        Ok(Self {
            root,
            oms,
            threads,
            repo,
        })
    }
    pub fn check(&self) -> Result<()> {
        self.root.check()?;
        self.oms.check()?;
        self.threads.check()
    }
    pub fn name(id: &str) -> OsString {
        OsString::from(format!("{id}.jsonl"))
    }
    pub fn path(&self, id: &str) -> PathBuf {
        self.repo.join(".oms/threads").join(Self::name(id))
    }
    pub fn snapshot(&self, id: &str) -> Result<Snapshot> {
        self.check()?;
        let (bytes, stat) = self.threads.read(&Self::name(id), MAX_FILE)?;
        self.check()?;
        Snapshot::decode(id, bytes, stat)
    }
    pub fn lock(&self, id: &str, stop: &AtomicBool) -> Result<oms_lock::Guard> {
        self.check()?;
        let settings = oms_lock::Settings::from_env().map_err(io)?;
        let lock = oms_lock::Lock::for_file(&self.path(id), settings).map_err(io)?;
        Self::acquire(&lock, stop)
    }
    pub fn lock_before(
        &self,
        id: &str,
        stop: &AtomicBool,
        deadline: Instant,
    ) -> Result<oms_lock::Guard> {
        checkpoint(stop)?;
        if Instant::now() >= deadline {
            return Err(Error::new("room lock deadline expired"));
        }
        self.check()?;
        let mut settings = oms_lock::Settings::from_env().map_err(io)?;
        settings.timeout = settings
            .timeout
            .min(deadline.saturating_duration_since(Instant::now()));
        let lock = oms_lock::Lock::for_file(&self.path(id), settings).map_err(io)?;
        let guard = Self::acquire(&lock, stop)?;
        if Instant::now() >= deadline {
            return Err(Error::new("room lock deadline expired"));
        }
        Ok(guard)
    }
    pub fn acquire(lock: &oms_lock::Lock, stop: &AtomicBool) -> Result<oms_lock::Guard> {
        checkpoint(stop)?;
        let cancellation = oms_lock::Cancellation::default();
        let finished = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let watcher = scope.spawn(|| {
                while !finished.load(Ordering::Acquire) {
                    if stop.load(Ordering::Acquire) {
                        cancellation.cancel();
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(25));
                }
            });
            let result = lock.acquire(Some(&cancellation)).map_err(io);
            finished.store(true, Ordering::Release);
            watcher.thread().unpark();
            result
        })
    }
    pub fn rollover_lock(&self, id: &str, stop: &AtomicBool) -> Result<PanelGuard> {
        self.root.check()?;
        self.oms.check()?;
        let artifacts = self.oms.child(OsStr::new("artifacts"), true)?;
        let mut name = format!("room-rollover-{id}");
        if name.len() > 160 {
            name = format!("room-rollover-{}", &crate::digest(id.as_bytes())[..32]);
        }
        let name = OsString::from(format!("{name}.lock"));
        let fd = fs::openat(
            &artifacts.fd,
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        if !regular(&initial) {
            return Err(Error::new("unsafe panel lock"));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            checkpoint(stop)?;
            match fs::flock(&fd, fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(Errno::WOULDBLOCK | Errno::INTR) => {
                    if Instant::now() >= deadline {
                        return Err(Error::new("panel operation is busy; retry later"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(io(e)),
            }
        }
        let guard = PanelGuard {
            directory: artifacts,
            fd,
            name,
            initial,
        };
        guard.check()?;
        Ok(guard)
    }
}
pub struct PanelGuard {
    directory: Directory,
    fd: OwnedFd,
    name: OsString,
    initial: Stat,
}
impl PanelGuard {
    pub fn check(&self) -> Result<()> {
        self.directory.check()?;
        let named =
            fs::statat(&self.directory.fd, &self.name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
        if !same(&self.initial, &named)
            || !regular(&named)
            || !same(&self.initial, &fs::fstat(&self.fd).map_err(io)?)
        {
            return Err(Error::new("panel lock generation changed"));
        }
        Ok(())
    }
}
pub struct Snapshot {
    pub id: String,
    pub bytes: Vec<u8>,
    pub rows: Vec<Value>,
    pub ranges: Vec<Range<usize>>,
    pub stat: Stat,
}
impl Snapshot {
    pub fn decode(id: &str, bytes: Vec<u8>, stat: Stat) -> Result<Self> {
        if bytes.len() > MAX_FILE || !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(Error::new(
                "room log is oversized or has an incomplete turn",
            ));
        }
        let mut rows = Vec::new();
        let mut ranges = Vec::new();
        let mut start = 0;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let end = start + line.len();
            if line.len() > MAX_ROW {
                return Err(Error::new("oversized room row"));
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                let row = parse(line, MAX_ROW)?;
                if row.as_object().is_none()
                    || row.text("thread") != id
                    || row["seq"].as_i64().is_none()
                    || row
                        .get("room_event")
                        .is_some_and(|v| v.as_object().is_none())
                {
                    return Err(Error::new("invalid room log"));
                }
                rows.push(row);
                ranges.push(start..end);
            }
            start = end;
        }
        Ok(Self {
            id: id.to_owned(),
            bytes,
            rows,
            ranges,
            stat,
        })
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    #[test]
    fn unchanged_ticks_read_nothing_and_growth_seeks_to_tail() {
        let path = std::env::temp_dir().join(format!("oms-room-tail-{}", crate::unique().unwrap()));
        std::fs::create_dir(&path).unwrap();
        let result = std::panic::catch_unwind(|| {
            let directory = Directory::open(&path).unwrap();
            let name = OsStr::new("log");
            let line = b"{\"thread\":\"room\",\"seq\":1}\n";
            let bytes = line.repeat(100);
            directory.create(name, &bytes).unwrap();
            let (bytes, stat) = directory.read(name, MAX_FILE).unwrap();
            let snapshot = Snapshot::decode("room", bytes, stat).unwrap();
            let mut reader = LogReader::open(&directory, name.to_owned()).unwrap();
            assert!(reader.poll(&snapshot).unwrap().is_none());
            assert_eq!(reader.bytes_read, 0);
            directory.append(name, &snapshot.stat, line).unwrap();
            let updated = reader.poll(&snapshot).unwrap().unwrap();
            assert_eq!(reader.bytes_read, 512 + line.len());
            assert_eq!(updated.rows.len(), 101);
            assert_eq!(updated.ranges.last().unwrap().start, snapshot.bytes.len());
            assert!(reader.poll(&updated).unwrap().is_none());
            assert_eq!(reader.bytes_read, 512 + line.len());
            std::fs::write(path.join(name), b"{}\n").unwrap();
            assert!(reader.refresh(&updated).is_err());
            let replacement = OsStr::new("next");
            directory.create(replacement, line).unwrap();
            std::fs::rename(path.join(replacement), path.join(name)).unwrap();
            let replaced = reader.refresh(&updated).unwrap().unwrap();
            assert_eq!(replaced.rows.len(), 1);
            assert!(!same(&replaced.stat, &updated.stat));
            let bounded = OsStr::new("bounded");
            directory.create(bounded, b"").unwrap();
            let bounded_path = path.join(bounded);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&bounded_path)
                .unwrap();
            assert!(LogReader::open(&directory, bounded.to_owned()).is_ok());
            file.set_len(MAX_FILE as u64).unwrap();
            assert!(LogReader::open(&directory, bounded.to_owned()).is_ok());
            file.set_len(MAX_FILE as u64 + 1).unwrap();
            assert!(LogReader::open(&directory, bounded.to_owned()).is_err());
            let invalid = b"{\"thread\":\"room\",\"seq\":2}";
            directory.append(name, &replaced.stat, invalid).unwrap();
            assert!(reader.refresh(&replaced).is_err());
        });
        std::fs::remove_dir_all(path).unwrap();
        result.unwrap();
    }
}
