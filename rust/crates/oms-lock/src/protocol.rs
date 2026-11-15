use crate::identity::{self, Observation};
use crate::{
    Cancellation, Error, Lock, Mode as Selection, Status, paths, sleep_interval, wait_until,
};
use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READ_LIMIT: u64 = 65536;
const META: [&str; 4] = ["owner", "pid", "process-start", "started"];
const CLAIM_META: [&str; 3] = ["choosing", "generation", "ticket"];
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
// Legacy bakery ties by numeric PID. Native threads share a PID, so serialize
// just their recovery elections (never callbacks or acquisition waits).
type RecoveryKey = (String, OsString);
type RecoveryRegistry = Mutex<HashMap<RecoveryKey, Weak<Mutex<()>>>>;
static RECLAIMERS: OnceLock<RecoveryRegistry> = OnceLock::new();

fn recovery_mutex(root: &Root, name: &OsStr) -> Result<Arc<Mutex<()>>, Error> {
    let mut registry = RECLAIMERS.get_or_init(Mutex::default).lock().map_err(io)?;
    registry.retain(|_, value| value.strong_count() != 0);
    let key = (
        format!("{}:{}", root.identity.st_dev, root.identity.st_ino),
        name.to_owned(),
    );
    if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    let mutex = Arc::new(Mutex::new(()));
    registry.insert(key, Arc::downgrade(&mutex));
    Ok(mutex)
}

fn io(_: impl std::fmt::Debug) -> Error {
    Error::Io
}
fn observation_error(error: Errno) -> Error {
    // Disappearance invalidates this attempt, never proves a claimant dead.
    if error == Errno::NOENT {
        Error::Changed
    } else {
        Error::Io
    }
}
fn now() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(io)
}
fn unique() -> Result<String, Error> {
    Ok(format!(
        "{}.{}.{}",
        std::process::id(),
        now()?,
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}
fn same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn file_kind(s: &Stat) -> FileType {
    FileType::from_raw_mode(s.st_mode)
}
fn owned(s: &Stat) -> bool {
    s.st_uid == rustix::process::geteuid().as_raw() && s.st_mode & 0o022 == 0
}
fn open_dir(fd: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Error> {
    let directory = fs::openat(
        fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        if matches!(e, Errno::LOOP | Errno::NOTDIR) {
            Error::Unsafe
        } else {
            observation_error(e)
        }
    })?;
    #[cfg(test)]
    tests::observe("directory-opened", name);
    Ok(directory)
}

#[derive(Debug)]
struct AnchorChain {
    caller_path: PathBuf,
    physical_path: PathBuf,
    fd: OwnedFd,
    stat: Stat,
    segments: Vec<(OsString, Stat)>,
}

#[derive(Debug)]
struct Root {
    fd: OwnedFd,
    path: PathBuf,
    identity: Stat,
    anchor: Option<AnchorChain>,
}
impl Root {
    fn open(path: &Path, create: bool) -> Result<Self, Error> {
        // Follow already-existing ancestor aliases only. Never mkdir through a
        // missing component that can be replaced by a symlink.
        let mut ancestor = path;
        let mut missing = Vec::new();
        loop {
            match std::fs::symlink_metadata(ancestor) {
                Ok(m) => {
                    if !m.is_dir() || m.file_type().is_symlink() {
                        return Err(Error::Unsafe);
                    }
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                    missing.push(ancestor.file_name().ok_or(Error::Unsafe)?.to_owned());
                    ancestor = ancestor.parent().ok_or(Error::Unsafe)?;
                }
                Err(_) => return Err(Error::Io),
            }
        }
        let physical = std::fs::canonicalize(ancestor).map_err(io)?;
        let mut fd = fs::open(
            &physical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        for name in missing.iter().rev() {
            match fs::mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(_) => return Err(Error::Io),
            }
            fd = open_dir(&fd, name)?;
            if !owned(&fs::fstat(&fd).map_err(io)?) {
                return Err(Error::Unsafe);
            }
        }
        let identity = fs::fstat(&fd).map_err(io)?;
        if !owned(&identity) {
            return Err(Error::Unsafe);
        }
        let root = Self {
            fd,
            path: path.to_owned(),
            identity,
            anchor: None,
        };
        root.check()?;
        Ok(root)
    }
    fn open_in_anchor(
        anchor_path: &Path,
        parent_path: &Path,
        create: bool,
    ) -> Result<Option<Self>, Error> {
        let rel_parent = parent_path
            .strip_prefix(anchor_path)
            .map_err(|_| Error::InvalidTarget)?;
        let caller_stat = match fs::statat(fs::CWD, anchor_path, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) if file_kind(&st) == FileType::Directory => st,
            Ok(_) | Err(Errno::LOOP | Errno::NOTDIR) => return Err(Error::Unsafe),
            Err(_) => return Err(Error::Io),
        };
        let physical_anchor = std::fs::canonicalize(anchor_path).map_err(io)?;
        let anchor_fd = fs::open(
            &physical_anchor,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let anchor_stat = fs::fstat(&anchor_fd).map_err(io)?;
        if file_kind(&anchor_stat) != FileType::Directory {
            return Err(Error::Unsafe);
        }
        if !same(&caller_stat, &anchor_stat) {
            return Err(Error::Changed);
        }
        let mut segs = Vec::new();
        for component in rel_parent.components() {
            match component {
                Component::Normal(seg) => segs.push(seg.to_owned()),
                _ => return Err(Error::InvalidTarget),
            }
        }
        let mut fd = anchor_fd.try_clone().map_err(io)?;
        let mut recorded = Vec::with_capacity(segs.len());
        let len = segs.len();
        for (idx, seg) in segs.into_iter().enumerate() {
            let is_leaf_parent = idx + 1 == len;
            let mut created = false;
            if create {
                match fs::mkdirat(&fd, &seg, Mode::from_raw_mode(0o700)) {
                    Ok(()) => created = true,
                    Err(Errno::EXIST) => {}
                    Err(Errno::LOOP | Errno::NOTDIR) => return Err(Error::Unsafe),
                    Err(_) => return Err(Error::Io),
                }
            } else {
                match fs::statat(&fd, &seg, AtFlags::SYMLINK_NOFOLLOW) {
                    Err(Errno::NOENT) => return Ok(None),
                    Err(Errno::LOOP | Errno::NOTDIR) => return Err(Error::Unsafe),
                    Err(_) => return Err(Error::Io),
                    Ok(st)
                        if file_kind(&st) != FileType::Directory
                            || (is_leaf_parent && !owned(&st)) =>
                    {
                        return Err(Error::Unsafe);
                    }
                    Ok(_) => {}
                }
            }
            fd = open_dir(&fd, &seg)?;
            let st = fs::fstat(&fd).map_err(io)?;
            if file_kind(&st) != FileType::Directory || ((created || is_leaf_parent) && !owned(&st))
            {
                return Err(Error::Unsafe);
            }
            recorded.push((seg, st));
        }
        let identity = fs::fstat(&fd).map_err(io)?;
        if file_kind(&identity) != FileType::Directory || !owned(&identity) {
            return Err(Error::Unsafe);
        }
        let root = Self {
            fd,
            path: physical_anchor.join(rel_parent),
            identity,
            anchor: Some(AnchorChain {
                caller_path: anchor_path.to_owned(),
                physical_path: physical_anchor,
                fd: anchor_fd,
                stat: anchor_stat,
                segments: recorded,
            }),
        };
        root.check()?;
        Ok(Some(root))
    }
    fn check(&self) -> Result<(), Error> {
        if let Some(chain) = &self.anchor {
            for anchor_name in std::iter::once(&chain.caller_path)
                .chain((chain.physical_path != chain.caller_path).then_some(&chain.physical_path))
            {
                let named_anchor = fs::statat(fs::CWD, anchor_name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|e| {
                        if matches!(e, Errno::LOOP | Errno::NOTDIR) {
                            Error::Unsafe
                        } else {
                            Error::Changed
                        }
                    })?;
                if file_kind(&named_anchor) != FileType::Directory {
                    return Err(Error::Unsafe);
                }
                if !same(&named_anchor, &chain.stat) {
                    return Err(Error::Changed);
                }
            }
            let current_anchor = fs::fstat(&chain.fd).map_err(io)?;
            if !same(&current_anchor, &chain.stat) {
                return Err(Error::Changed);
            }
            if file_kind(&current_anchor) != FileType::Directory {
                return Err(Error::Unsafe);
            }
            let mut dir = chain.fd.try_clone().map_err(io)?;
            for (idx, (seg, recorded_stat)) in chain.segments.iter().enumerate() {
                let named = fs::statat(&dir, seg, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| {
                    if matches!(e, Errno::LOOP | Errno::NOTDIR) {
                        Error::Unsafe
                    } else {
                        Error::Changed
                    }
                })?;
                if file_kind(&named) != FileType::Directory {
                    return Err(Error::Unsafe);
                }
                if !same(&named, recorded_stat) {
                    return Err(Error::Changed);
                }
                dir = match open_dir(&dir, seg) {
                    Ok(next) => next,
                    Err(Error::Unsafe) => return Err(Error::Unsafe),
                    Err(_) => return Err(Error::Changed),
                };
                let opened = fs::fstat(&dir).map_err(io)?;
                if !same(&opened, recorded_stat) {
                    return Err(Error::Changed);
                }
                if file_kind(&opened) != FileType::Directory
                    || (idx + 1 == chain.segments.len() && !owned(&opened))
                {
                    return Err(Error::Unsafe);
                }
            }
            let current = fs::fstat(&dir).map_err(io)?;
            let descriptor = fs::fstat(&self.fd).map_err(io)?;
            if !same(&current, &self.identity) || !same(&descriptor, &self.identity) {
                return Err(Error::Changed);
            }
            if file_kind(&current) != FileType::Directory || !owned(&current) || !owned(&descriptor)
            {
                return Err(Error::Unsafe);
            }
            return Ok(());
        }
        let current = fs::statat(fs::CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::Changed)?;
        let descriptor = fs::fstat(&self.fd).map_err(io)?;
        if !same(&current, &self.identity) || !same(&descriptor, &self.identity) {
            return Err(Error::Changed);
        }
        if file_kind(&current) != FileType::Directory || !owned(&current) || !owned(&descriptor) {
            return Err(Error::Unsafe);
        }
        Ok(())
    }
    fn stat(&self, name: &OsStr) -> Result<Stat, Errno> {
        fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW)
    }
    fn directory(&self, name: &OsStr) -> Result<Directory, Error> {
        Directory::open(&self.fd, name)
    }
}

#[derive(Debug)]
struct Directory {
    fd: OwnedFd,
    identity: Stat,
}
impl Directory {
    fn open(parent: &OwnedFd, name: &OsStr) -> Result<Self, Error> {
        let fd = open_dir(parent, name)?;
        let identity = fs::fstat(&fd).map_err(io)?;
        let named =
            fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(observation_error)?;
        if !same(&identity, &named) {
            return Err(Error::Changed);
        }
        if file_kind(&identity) != FileType::Directory || !owned(&identity) || !owned(&named) {
            return Err(Error::Unsafe);
        }
        Ok(Self { fd, identity })
    }
    fn check(&self, root: &Root, name: &OsStr) -> Result<(), Error> {
        root.check()?;
        let current = root.stat(name).map_err(|_| Error::Changed)?;
        if !same(&current, &self.identity) {
            return Err(Error::Changed);
        }
        if !owned(&current) {
            return Err(Error::Unsafe);
        }
        Ok(())
    }
    fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        let fd = match fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(Vec::new()),
            Err(Errno::LOOP) => return Err(Error::Unsafe),
            Err(_) => return Err(Error::Io),
        };
        #[cfg(test)]
        tests::observe("read-opened", OsStr::new(name));
        let before = fs::fstat(&fd).map_err(io)?;
        if before.st_nlink == 0 {
            return Err(Error::Changed);
        }
        if file_kind(&before) != FileType::RegularFile || !owned(&before) || before.st_nlink != 1 {
            return Err(Error::Unsafe);
        }
        let mut bytes = Vec::new();
        let mut file = File::from(fd);
        // Only the first line participates, as with sed -n 1p; bound foreign input.
        BufReader::new((&mut file).take(READ_LIMIT + 1))
            .read_until(b'\n', &mut bytes)
            .map_err(io)?;
        if bytes.len() as u64 > READ_LIMIT {
            return Err(Error::Unsafe);
        }
        #[cfg(test)]
        tests::observe("read-complete", OsStr::new(name));
        let after = fs::fstat(&file).map_err(io)?;
        let named =
            fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Changed)?;
        if !same(&before, &named)
            || !same(&before, &after)
            || before.st_size != after.st_size
            || before.st_mtime != after.st_mtime
            || before.st_mtime_nsec != after.st_mtime_nsec
            || !owned(&after)
            || after.st_nlink != 1
        {
            return Err(Error::Changed);
        }
        while bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        Ok(bytes)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?;
        let mut file = File::from(fd);
        file.write_all(bytes).map_err(io)?;
        file.write_all(b"\n").map_err(io)?;
        file.sync_all().map_err(io)
    }
    fn replace_choosing(&self) -> Result<(), Error> {
        // Our claim is private; use one descriptor and validate before truncation.
        let fd = fs::openat(
            &self.fd,
            "choosing",
            OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let s = fs::fstat(&fd).map_err(io)?;
        if file_kind(&s) != FileType::RegularFile || s.st_nlink != 1 || !owned(&s) {
            return Err(Error::Unsafe);
        }
        let mut file = File::from(fd);
        file.write_all(b"0\n").map_err(io)?;
        file.sync_all().map_err(io)
    }
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(Metadata {
            owner: self.read("owner")?,
            pid: self.read("pid")?,
            start: self.read("process-start")?,
            started: self.read("started")?,
        })
    }
    fn entries(&self) -> Result<Vec<OsString>, Error> {
        let mut entries = Vec::new();
        for entry in fs::Dir::read_from(&self.fd).map_err(io)? {
            let entry = entry.map_err(io)?;
            let bytes = entry.file_name().to_bytes();
            if bytes != b"." && bytes != b".." {
                entries.push(OsString::from_vec(bytes.to_vec()));
            }
        }
        Ok(entries)
    }
    fn validate_entries(&self, names: &[&str]) -> Result<Vec<OsString>, Error> {
        // Never recursive-delete unexpected entries or follow foreign links.
        let entries = self.entries()?;
        if entries
            .iter()
            .any(|entry| !names.iter().any(|n| entry == n))
        {
            return Err(Error::Unsafe);
        }
        for entry in &entries {
            let stat = fs::statat(&self.fd, entry, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(observation_error)?;
            if file_kind(&stat) != FileType::RegularFile || !owned(&stat) || stat.st_nlink != 1 {
                return Err(Error::Unsafe);
            }
        }
        Ok(entries)
    }
    fn clean(&self, names: &[&str]) -> Result<(), Error> {
        for entry in self.validate_entries(names)? {
            fs::unlinkat(&self.fd, entry, AtFlags::empty()).map_err(observation_error)?;
        }
        fs::fsync(&self.fd).map_err(io)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    owner: Vec<u8>,
    pid: Vec<u8>,
    start: Vec<u8>,
    started: Vec<u8>,
}
fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}
fn process_alive(pid: u64) -> Result<bool, Error> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .and_then(rustix::process::Pid::from_raw)
        .ok_or(Error::Unsafe)?;
    match rustix::process::test_kill_process(pid) {
        Ok(()) => Ok(true),
        Err(Errno::SRCH) => Ok(false),
        // EPERM does not establish death (legacy kill -0 is weaker here).
        Err(_) => Err(Error::Io),
    }
}
fn stale(
    directory: &Directory,
    metadata: &Metadata,
    timeout: Duration,
    current_time: u64,
    observation: Observation<'_>,
) -> Result<bool, Error> {
    if let Some(pid) = decimal(&metadata.pid) {
        if !process_alive(pid)? {
            return Ok(true);
        }
        if !metadata.start.is_empty()
            && let Some(current) = identity::start(pid, observation)?
            && current != metadata.start
        {
            return Ok(true);
        }
        return Ok(false);
    }
    if metadata.owner.is_empty() && metadata.pid.is_empty() && metadata.started.is_empty() {
        let stat = fs::fstat(&directory.fd).map_err(io)?;
        // find -mmin +1 truncates minute age: first true at 120 seconds.
        let modified = UNIX_EPOCH
            .checked_add(Duration::new(
                stat.st_mtime.try_into().map_err(io)?,
                stat.st_mtime_nsec.try_into().map_err(io)?,
            ))
            .ok_or(Error::Unsafe)?;
        return Ok(ownerless_old(modified, SystemTime::now()));
    }
    Ok(!metadata.owner.is_empty()
        && decimal(&metadata.started).is_some_and(|started| {
            current_time >= started
                && current_time - started
                    >= timeout
                        .as_secs()
                        .saturating_add(u64::from(timeout.subsec_nanos() != 0))
        }))
}

fn ownerless_old(modified: SystemTime, current: SystemTime) -> bool {
    current
        .duration_since(modified)
        .is_ok_and(|age| age >= Duration::from_secs(120))
}

/// A non-reentrant owned guard. Borrow `&Guard` across nested callbacks.
/// Descriptors are CLOEXEC; a child exec never inherits ownership implicitly.
#[derive(Debug)]
pub struct Guard {
    root: Root,
    name: OsString,
    held: Held,
    pid: u32,
    active: bool,
}
#[derive(Debug)]
enum Held {
    Flock(OwnedFd, Stat),
    Mkdir(Directory, Vec<u8>),
}
impl Guard {
    /// Best-effort Drop is supplemented by an explicit observable release.
    pub fn release(mut self) -> Result<(), Error> {
        self.active = false;
        self.release_inner()
    }
    /// Revalidate path identity before a protected write after lengthy work.
    pub fn check(&self) -> Result<(), Error> {
        if self.pid != std::process::id() {
            return Err(Error::Changed);
        }
        self.root.check()?;
        match &self.held {
            Held::Flock(fd, identity) => {
                let named = self.root.stat(&self.name).map_err(|_| Error::Changed)?;
                let current = fs::fstat(fd).map_err(io)?;
                if !same(identity, &named) || !same(identity, &current) {
                    return Err(Error::Changed);
                }
                if !owned(&named) || current.st_nlink != 1 {
                    return Err(Error::Unsafe);
                }
                Ok(())
            }
            Held::Mkdir(directory, owner) => {
                directory.check(&self.root, &self.name)?;
                if directory.read("owner")? != *owner {
                    return Err(Error::Changed);
                }
                Ok(())
            }
        }
    }
    fn release_inner(&self) -> Result<(), Error> {
        match &self.held {
            Held::Flock(fd, _) => {
                // Never unlink flock files; unlock only from the acquiring PID so a
                // forked non-exec child never unlocks its parent, while a concurrent
                // fork..exec on another thread cannot extend a released parent guard.
                if self.pid != std::process::id() {
                    return Err(Error::Changed);
                }
                loop {
                    match fs::flock(fd, FlockOperation::Unlock) {
                        Ok(()) => return Ok(()),
                        Err(Errno::INTR) => continue,
                        Err(err) => return Err(io(err)),
                    }
                }
            }
            Held::Mkdir(directory, _) => {
                self.check()?;
                directory.clean(&META)?;
                directory.check(&self.root, &self.name)?;
                fs::unlinkat(&self.root.fd, &self.name, AtFlags::REMOVEDIR).map_err(io)?;
                fs::fsync(&self.root.fd).map_err(io)
            }
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if self.active {
            let _ = self.release_inner();
        }
    }
}

fn flock_file(root: &Root, name: &OsStr) -> Result<(OwnedFd, Stat), Error> {
    let fd = fs::openat(
        &root.fd,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| {
        if matches!(e, Errno::LOOP | Errno::ISDIR) {
            Error::Unsafe
        } else {
            Error::Io
        }
    })?;
    let identity = fs::fstat(&fd).map_err(io)?;
    if file_kind(&identity) != FileType::RegularFile || !owned(&identity) || identity.st_nlink != 1
    {
        return Err(Error::Unsafe);
    }
    let named = root.stat(name).map_err(io)?;
    if !same(&named, &identity) {
        return Err(Error::Changed);
    }
    Ok((fd, identity))
}

pub(crate) fn acquire(
    lock: &Lock,
    cancellation: Option<&Cancellation>,
    nonblocking: bool,
) -> Result<Guard, Error> {
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return Err(Error::Cancelled);
    }
    let start = Instant::now();
    let deadline = start
        .checked_add(lock.settings.timeout)
        .ok_or(Error::InvalidSettings)?;
    let observation = Observation {
        deadline,
        cancellation,
    };
    let parent = lock.path.parent().ok_or(Error::InvalidTarget)?;
    let root = if let Some(anchor) = lock.anchor() {
        Root::open_in_anchor(anchor, parent, true)?.ok_or(Error::Io)?
    } else {
        Root::open(parent, true)?
    };
    let name = lock
        .path
        .file_name()
        .ok_or(Error::InvalidTarget)?
        .to_owned();
    let flock = if lock.mode == Selection::Flock {
        Some(flock_file(&root, &name)?)
    } else {
        None
    };
    let identity = if lock.mode == Selection::Mkdir {
        Some(
            identity::start(std::process::id() as u64, observation)?
                .ok_or(Error::UnsupportedIdentity)?,
        )
    } else {
        None
    };
    loop {
        if cancellation.is_some_and(Cancellation::is_cancelled) {
            return Err(Error::Cancelled);
        }
        root.check()?;
        if let Some((fd, stat)) = &flock {
            match fs::flock(fd, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    let identity = *stat;
                    let guard = Guard {
                        root,
                        name,
                        held: Held::Flock(flock.unwrap().0, identity),
                        pid: std::process::id(),
                        active: true,
                    };
                    guard.check()?;
                    return Ok(guard);
                }
                Err(Errno::WOULDBLOCK) | Err(Errno::INTR) => {}
                Err(_) => return Err(Error::Io),
            }
        } else {
            match fs::mkdirat(&root.fd, &name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let directory = root.directory(&name)?;
                    let owner = unique()?.into_bytes();
                    let guard = Guard {
                        root,
                        name,
                        held: Held::Mkdir(directory, owner),
                        pid: std::process::id(),
                        active: true,
                    };
                    if let Held::Mkdir(directory, owner) = &guard.held {
                        directory.write("owner", owner)?;
                        directory.write("pid", std::process::id().to_string().as_bytes())?;
                        directory.write("process-start", identity.as_ref().unwrap())?;
                        directory.write("started", now()?.to_string().as_bytes())?;
                        fs::fsync(&directory.fd).map_err(io)?;
                        fs::fsync(&guard.root.fd).map_err(io)?;
                    }
                    guard.check()?;
                    return Ok(guard);
                }
                Err(Errno::EXIST) => {
                    match reclaim(&root, &name, lock.settings.timeout, observation) {
                        Ok(true) => {
                            if Instant::now() >= deadline {
                                return Err(Error::Busy);
                            }
                            continue;
                        }
                        Ok(false) => {}
                        // A cooperating holder can release between observation and
                        // validation. Retry only with the same current root authority.
                        Err(Error::Changed) => {
                            root.check()?;
                        }
                        Err(Error::Io) if matches!(root.stat(&name), Err(Errno::NOENT)) => {
                            root.check()?;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(_) => return Err(Error::Io),
            }
        }
        let elapsed = start.elapsed();
        if nonblocking || Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let interval = sleep_interval(elapsed, remaining, lock.settings.poll_max);
        wait_until(Instant::now() + interval, cancellation)?;
    }
}

fn append(name: &OsStr, suffix: &str) -> OsString {
    let mut name = name.to_owned();
    name.push(suffix);
    name
}
fn claim_live(name: &OsStr, observation: Observation<'_>) -> Result<bool, Error> {
    let mut parts = name.as_bytes().split(|b| *b == b'.');
    let Some(pid) = parts.next().and_then(decimal) else {
        return Err(Error::Unsafe);
    };
    let Some(sum) = parts.next() else {
        return Err(Error::Unsafe);
    };
    if !process_alive(pid)? {
        return Ok(false);
    }
    if sum != b"none"
        && let Some(current) = identity::start(pid, observation)?
    {
        return Ok(paths::cksum(&current).as_bytes() == sum);
    }
    Ok(true)
}

fn reclaim(
    root: &Root,
    name: &OsStr,
    timeout: Duration,
    observation: Observation<'_>,
) -> Result<bool, Error> {
    let mutex = recovery_mutex(root, name)?;
    let _serial = match mutex.try_lock() {
        Ok(serial) => serial,
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Poisoned(_)) => return Err(Error::Io),
    };
    observation.check()?;
    if matches!(root.stat(name), Err(Errno::NOENT)) {
        return Ok(true);
    }
    let observed = match root.directory(name) {
        Ok(directory) => directory,
        Err(_) if matches!(root.stat(name), Err(Errno::NOENT)) => return Ok(true),
        Err(error) => return Err(error),
    };
    let metadata = observed.metadata()?;
    if !stale(&observed, &metadata, timeout, now()?, observation)? {
        return Ok(false);
    }
    observed.check(root, name)?;
    observed.validate_entries(&META)?;
    let queue_name = append(name, ".reclaim-queue");
    match fs::mkdirat(&root.fd, &queue_name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err(Error::Io),
    }
    let queue = root.directory(&queue_name)?;
    let start = identity::start(std::process::id() as u64, observation)?
        .ok_or(Error::UnsupportedIdentity)?;
    let claim_name = OsString::from(format!(
        "{}.{}.{}",
        std::process::id(),
        paths::cksum(&start),
        unique()?
    ));
    fs::mkdirat(&queue.fd, &claim_name, Mode::from_raw_mode(0o700)).map_err(io)?;
    let claim = Directory::open(&queue.fd, &claim_name)?;
    let result = elect_and_reclaim(
        root,
        name,
        (&observed, &metadata),
        &queue,
        (&claim, &claim_name),
        timeout,
        observation,
    );
    // A killed claimant stays observable by PID/start identity; never delete other claims.
    if claim.clean(&CLAIM_META).is_ok() {
        let current = fs::statat(&queue.fd, &claim_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(observation_error)?;
        if same(&claim.identity, &current) {
            let _ = fs::unlinkat(&queue.fd, &claim_name, AtFlags::REMOVEDIR);
        }
    }
    result
}

fn elect_and_reclaim(
    root: &Root,
    name: &OsStr,
    observed: (&Directory, &Metadata),
    queue: &Directory,
    claim: (&Directory, &OsStr),
    timeout: Duration,
    observation: Observation<'_>,
) -> Result<bool, Error> {
    let (observed, metadata) = observed;
    let (claim, claim_name) = claim;
    claim.write("choosing", b"1")?;
    // Generation file is consumed as an opaque identifier by legacy contenders.
    let mut generation = b"owner:".to_vec();
    generation.extend_from_slice(&metadata.owner);
    claim.write("generation", &generation)?;
    let mut maximum = 0;
    for other_name in queue.entries()? {
        observation.check()?;
        if other_name == claim_name || !claim_live(&other_name, observation)? {
            continue;
        }
        #[cfg(test)]
        tests::observe("maximum", &other_name);
        let other = Directory::open(&queue.fd, &other_name)?;
        if let Some(ticket) = decimal(&other.read("ticket")?) {
            maximum = maximum.max(ticket);
        }
    }
    let ticket = maximum.checked_add(1).ok_or(Error::Unsafe)?;
    claim.write("ticket", ticket.to_string().as_bytes())?;
    claim.replace_choosing()?;
    for other_name in queue.entries()? {
        observation.check()?;
        if other_name == claim_name || !claim_live(&other_name, observation)? {
            continue;
        }
        #[cfg(test)]
        tests::observe("election", &other_name);
        let other = Directory::open(&queue.fd, &other_name)?;
        let choosing = other.read("choosing")?;
        let other_ticket = decimal(&other.read("ticket")?);
        if choosing != b"0" || other_ticket.is_none() {
            return Ok(false);
        }
        let other_ticket = other_ticket.unwrap();
        let other_pid = decimal(
            other_name
                .as_bytes()
                .split(|b| *b == b'.')
                .next()
                .unwrap_or_default(),
        )
        .ok_or(Error::Unsafe)?;
        if other_ticket < ticket
            || (other_ticket == ticket && other_pid <= std::process::id() as u64)
        {
            return Ok(false);
        }
    }
    queue.check(root, &append(name, ".reclaim-queue"))?;
    observation.check()?;
    // The election only serializes reclaimers. Fresh inode AND metadata AND
    // stale proof protect the winner's newly acquired canonical generation.
    if observed.check(root, name).is_err() {
        return Ok(false);
    }
    let fresh = observed.metadata()?;
    if fresh != *metadata || !stale(observed, &fresh, timeout, now()?, observation)? {
        return Ok(false);
    }
    let quarantine = append(name, &format!(".stale.{}", unique()?));
    match root.stat(&quarantine) {
        Err(Errno::NOENT) => {}
        _ => return Err(Error::Unsafe),
    }
    observed.validate_entries(&META)?;
    observed.check(root, name)?;
    fs::renameat(&root.fd, name, &root.fd, &quarantine).map_err(observation_error)?;
    if !same(
        &root.stat(&quarantine).map_err(observation_error)?,
        &observed.identity,
    ) {
        return Err(Error::Changed);
    }
    observed.clean(&META)?;
    fs::unlinkat(&root.fd, &quarantine, AtFlags::REMOVEDIR).map_err(io)?;
    fs::fsync(&root.fd).map_err(io)?;
    Ok(true)
}

pub(crate) fn probe(lock: &Lock) -> Result<Status, Error> {
    // Missing parents are free only when no existing ancestor is unsafe.
    let observation = Observation {
        deadline: Instant::now() + Duration::from_secs(2),
        cancellation: None,
    };
    let parent = lock.path.parent().ok_or(Error::InvalidTarget)?;
    let root = if let Some(anchor) = lock.anchor() {
        match Root::open_in_anchor(anchor, parent, false)? {
            Some(root) => root,
            None => return Ok(Status::Free),
        }
    } else {
        if !parent.exists() {
            let mut ancestor = parent;
            while let Some(next) = ancestor.parent() {
                match std::fs::symlink_metadata(ancestor) {
                    Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                        return Err(Error::Unsafe);
                    }
                    Ok(_) => return Ok(Status::Free),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => ancestor = next,
                    Err(_) => return Err(Error::Io),
                }
            }
            return Err(Error::Io);
        }
        Root::open(parent, false)?
    };
    let name = lock.path.file_name().ok_or(Error::InvalidTarget)?;
    let stat = match root.stat(name) {
        Err(Errno::NOENT) => return Ok(Status::Free),
        Err(_) => return Err(Error::Io),
        Ok(s) => s,
    };
    if lock.mode == Selection::Mkdir {
        let dir = root.directory(name)?;
        let metadata = dir.metadata()?;
        let result = if stale(&dir, &metadata, lock.settings.timeout, now()?, observation)? {
            Status::Free
        } else {
            Status::Held
        };
        dir.check(&root, name)?;
        return Ok(result);
    }
    if file_kind(&stat) != FileType::RegularFile || !owned(&stat) || stat.st_nlink != 1 {
        return Err(Error::Unsafe);
    }
    #[cfg(target_os = "linux")]
    {
        let key = format!(
            "{:02x}:{:02x}:{}",
            fs::major(stat.st_dev),
            fs::minor(stat.st_dev),
            stat.st_ino
        );
        let file = File::open("/proc/locks").map_err(io)?;
        for line in BufReader::new(file).split(b'\n') {
            let line = line.map_err(io)?;
            let fields: Vec<_> = line
                .split(|b| b.is_ascii_whitespace())
                .filter(|v| !v.is_empty())
                .collect();
            if fields.get(1) == Some(&b"FLOCK".as_slice()) && fields.get(5) == Some(&key.as_bytes())
            {
                root.check()?;
                if !same(&stat, &root.stat(name).map_err(io)?) {
                    return Err(Error::Changed);
                }
                return Ok(Status::Held);
            }
        }
    }
    // Absent rows cannot establish free across PID namespaces or inherited FDs.
    Ok(Status::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Settings, legacy_seconds};
    use std::cell::RefCell;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::sync::atomic::AtomicBool;

    type ObservationHook = Box<dyn FnMut(&str, &OsStr)>;
    thread_local! {
        static OBSERVATION_HOOK: RefCell<Option<ObservationHook>> = RefCell::new(None);
    }
    pub(super) fn observe(stage: &str, name: &OsStr) {
        OBSERVATION_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(stage, name);
            }
        });
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("oms-claim-observation-{}", unique().unwrap()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            Self(path)
        }
        fn directory(path: impl AsRef<Path>) -> std::io::Result<()> {
            std::fs::DirBuilder::new().mode(0o700).create(path)
        }
        fn write(path: impl AsRef<Path>, bytes: &[u8]) {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn lock(&self) -> Lock {
            Lock::at_path(
                self.0.join("state.lock"),
                Settings {
                    mode: Selection::Mkdir,
                    timeout: Duration::from_secs(legacy_seconds(
                        std::env::var_os("OMS_TEST_CALL_TIMEOUT").as_deref(),
                        30,
                    )),
                    poll_max: Duration::from_secs(1),
                    lock_dir: None,
                    home: self.0.clone(),
                    temp_dir: self.0.clone(),
                    cwd: self.0.clone(),
                    search_path: OsString::new(),
                    force_mkdir: false,
                },
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            OBSERVATION_HOOK.with(|hook| *hook.borrow_mut() = None);
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn disappearing_claims_retry_with_fresh_canonical_authority() {
        let mut failures = Vec::new();
        for (pass, window, field) in [
            ("maximum", "maximum", ""),
            ("maximum", "directory-opened", ""),
            ("maximum", "read-opened", "ticket"),
            ("maximum", "read-complete", "ticket"),
            ("election", "election", ""),
            ("election", "directory-opened", ""),
            ("election", "read-opened", "choosing"),
            ("election", "read-complete", "choosing"),
            ("election", "read-opened", "ticket"),
            ("election", "read-complete", "ticket"),
        ] {
            for replacement in [false, true] {
                let fixture = Fixture::new();
                let lock = fixture.lock();
                Fixture::directory(lock.path()).unwrap();
                let owner = "stale";
                Fixture::write(lock.path().join("owner"), owner.as_bytes());
                Fixture::write(lock.path().join("pid"), b"2147483647");
                Fixture::write(lock.path().join("started"), b"1");
                let queue = fixture.0.join("state.lock.reclaim-queue");
                Fixture::directory(&queue).unwrap();
                let other_name = OsString::from(format!("{}.none.other", std::process::id()));
                let other = queue.join(&other_name);
                Fixture::directory(&other).unwrap();
                Fixture::write(other.join("choosing"), b"0");
                Fixture::write(other.join("ticket"), b"1");
                let canonical = lock.path().to_owned();
                let fired = Arc::new(AtomicBool::new(false));
                let observed = fired.clone();
                let mut phase = "";
                OBSERVATION_HOOK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move |stage, name| {
                        if matches!(stage, "maximum" | "election") && name == other_name {
                            phase = if stage == "maximum" {
                                "maximum"
                            } else {
                                "election"
                            };
                        }
                        if phase == pass
                            && stage == window
                            && (name == other_name || name == OsStr::new(field))
                            && !observed.swap(true, Ordering::SeqCst)
                        {
                            // The claimant stays alive while withdrawing its own claim.
                            std::fs::remove_dir_all(&other).unwrap();
                            if replacement {
                                std::fs::remove_dir_all(&canonical).unwrap();
                                Fixture::directory(&canonical).unwrap();
                                Fixture::write(canonical.join("owner"), b"fresh-live");
                                Fixture::write(
                                    canonical.join("pid"),
                                    std::process::id().to_string().as_bytes(),
                                );
                                Fixture::write(canonical.join("started"), b"1");
                            }
                        }
                    }))
                });
                let result = if replacement {
                    lock.try_acquire()
                } else {
                    lock.acquire(None)
                };
                assert!(
                    fired.load(Ordering::SeqCst),
                    "{pass}/{window}/{field} hook missed: {result:?}"
                );
                let result = result.and_then(Guard::release);
                let expected = if replacement {
                    Err(Error::Busy)
                } else {
                    Ok(())
                };
                if result != expected {
                    failures.push(format!(
                        "{pass}/{window}/{field} replacement={replacement}: {result:?}"
                    ));
                }
                if replacement {
                    assert_eq!(
                        std::fs::read(lock.path().join("owner")).unwrap(),
                        b"fresh-live"
                    );
                    assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn ownerless_exact_age_boundary_and_future_time() {
        let modified = UNIX_EPOCH + Duration::from_secs(1);
        assert!(!ownerless_old(modified, modified));
        assert!(!ownerless_old(modified, UNIX_EPOCH));
        assert!(!ownerless_old(
            modified,
            modified + Duration::from_nanos(119_999_999_999)
        ));
        assert!(ownerless_old(modified, modified + Duration::from_secs(120)));
    }

    #[test]
    fn in_root_refuses_symlinked_oms_ancestor_without_touching_outside() {
        let fixture = Fixture::new();
        let repo = fixture.0.join("repo");
        let outside = fixture.0.join("outside");
        Fixture::directory(&repo).unwrap();
        Fixture::directory(&outside).unwrap();
        Fixture::directory(outside.join("artifacts")).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".oms")).unwrap();
        for mode in [Selection::Flock, Selection::Mkdir] {
            let lock = Lock::in_root(
                repo.clone(),
                Path::new(".oms/artifacts/panel-main-start.lock"),
                Settings {
                    mode,
                    timeout: Duration::from_secs(1),
                    poll_max: Duration::from_millis(50),
                    lock_dir: None,
                    home: fixture.0.clone(),
                    temp_dir: fixture.0.clone(),
                    cwd: repo.clone(),
                    search_path: OsString::new(),
                    force_mkdir: false,
                },
            )
            .unwrap();
            assert_eq!(lock.acquire(None).unwrap_err(), Error::Unsafe);
            assert!(
                std::fs::read_dir(outside.join("artifacts"))
                    .unwrap()
                    .next()
                    .is_none(),
                "outside/artifacts must stay empty for {mode:?}"
            );
        }

        // Group-writable (0775) repo root and pre-existing .oms are allowed as long as
        // newly created dirs / final lock parent directory (.oms/artifacts) remain 0700.
        for case in [
            "missing_artifacts",
            "existing_0775_oms",
            "loose_leaf_parent",
        ] {
            for mode in [Selection::Flock, Selection::Mkdir] {
                let shared_repo = fixture.0.join(format!("shared-{case}-{mode:?}"));
                Fixture::directory(&shared_repo).unwrap();
                std::fs::set_permissions(&shared_repo, std::fs::Permissions::from_mode(0o775))
                    .unwrap();
                if case != "missing_artifacts" {
                    let oms = shared_repo.join(".oms");
                    Fixture::directory(&oms).unwrap();
                    std::fs::set_permissions(&oms, std::fs::Permissions::from_mode(0o775)).unwrap();
                    let artifacts = oms.join("artifacts");
                    Fixture::directory(&artifacts).unwrap();
                    if case == "loose_leaf_parent" {
                        std::fs::set_permissions(
                            &artifacts,
                            std::fs::Permissions::from_mode(0o775),
                        )
                        .unwrap();
                    }
                }
                let lock = Lock::in_root(
                    shared_repo.clone(),
                    Path::new(".oms/artifacts/panel-main-start.lock"),
                    Settings {
                        mode,
                        timeout: Duration::from_secs(1),
                        poll_max: Duration::from_millis(50),
                        lock_dir: None,
                        home: fixture.0.clone(),
                        temp_dir: fixture.0.clone(),
                        cwd: shared_repo.clone(),
                        search_path: OsString::new(),
                        force_mkdir: false,
                    },
                )
                .unwrap();
                if case == "loose_leaf_parent" {
                    assert_eq!(
                        lock.acquire(None).unwrap_err(),
                        Error::Unsafe,
                        "{case} {mode:?}"
                    );
                } else {
                    let guard = lock.acquire(None).unwrap();
                    assert_eq!(lock.probe(), Status::Held, "{case} {mode:?}");
                    guard.release().unwrap();
                    assert_eq!(
                        lock.probe(),
                        if mode == Selection::Mkdir {
                            Status::Free
                        } else {
                            Status::Unknown
                        },
                        "{case} {mode:?}"
                    );
                }
            }
        }

        // Replacing an anchored root directory or retargeting an ancestor alias must
        // invalidate held guards without touching the replacement directory.
        for mode in [Selection::Flock, Selection::Mkdir] {
            let shared_repo = fixture.0.join(format!("replaced-anchor-{mode:?}"));
            let moved_repo = fixture.0.join(format!("moved-anchor-{mode:?}"));
            Fixture::directory(&shared_repo).unwrap();
            std::fs::set_permissions(&shared_repo, std::fs::Permissions::from_mode(0o775)).unwrap();
            let lock = Lock::in_root(
                shared_repo.clone(),
                Path::new(".oms/artifacts/panel-main-start.lock"),
                Settings {
                    mode,
                    timeout: Duration::from_secs(1),
                    poll_max: Duration::from_millis(50),
                    lock_dir: None,
                    home: fixture.0.clone(),
                    temp_dir: fixture.0.clone(),
                    cwd: shared_repo.clone(),
                    search_path: OsString::new(),
                    force_mkdir: false,
                },
            )
            .unwrap();
            let guard = lock.acquire(None).unwrap();
            std::fs::rename(&shared_repo, &moved_repo).unwrap();
            Fixture::directory(&shared_repo).unwrap();
            std::fs::set_permissions(&shared_repo, std::fs::Permissions::from_mode(0o775)).unwrap();
            assert_eq!(guard.check(), Err(Error::Changed), "replace check {mode:?}");
            assert_eq!(
                guard.release(),
                if mode == Selection::Mkdir {
                    Err(Error::Changed)
                } else {
                    Ok(())
                },
                "replace release {mode:?}"
            );
            assert!(
                std::fs::read_dir(&shared_repo).unwrap().next().is_none(),
                "replacement anchor stays untouched for {mode:?}"
            );
            lock.acquire(None).unwrap().release().unwrap();

            let real_a = fixture.0.join(format!("real-a-{mode:?}"));
            let real_b = fixture.0.join(format!("real-b-{mode:?}"));
            let alias_parent = fixture.0.join(format!("alias-parent-{mode:?}"));
            Fixture::directory(&real_a).unwrap();
            Fixture::directory(&real_b).unwrap();
            Fixture::directory(real_a.join("repo")).unwrap();
            Fixture::directory(real_b.join("repo")).unwrap();
            std::fs::set_permissions(real_a.join("repo"), std::fs::Permissions::from_mode(0o775))
                .unwrap();
            std::fs::set_permissions(real_b.join("repo"), std::fs::Permissions::from_mode(0o775))
                .unwrap();
            std::os::unix::fs::symlink(&real_a, &alias_parent).unwrap();
            let alias_repo = alias_parent.join("repo");
            let alias_lock = Lock::in_root(
                alias_repo.clone(),
                Path::new(".oms/artifacts/panel-main-start.lock"),
                Settings {
                    mode,
                    timeout: Duration::from_secs(1),
                    poll_max: Duration::from_millis(50),
                    lock_dir: None,
                    home: fixture.0.clone(),
                    temp_dir: fixture.0.clone(),
                    cwd: fixture.0.clone(),
                    search_path: OsString::new(),
                    force_mkdir: false,
                },
            )
            .unwrap();
            let alias_guard = alias_lock.acquire(None).unwrap();
            std::fs::remove_file(&alias_parent).unwrap();
            std::os::unix::fs::symlink(&real_b, &alias_parent).unwrap();
            assert_eq!(
                alias_guard.check(),
                Err(Error::Changed),
                "retargeted alias check {mode:?}"
            );
            assert_eq!(
                alias_guard.release(),
                if mode == Selection::Mkdir {
                    Err(Error::Changed)
                } else {
                    Ok(())
                },
                "retargeted alias release {mode:?}"
            );
            assert!(
                std::fs::read_dir(real_b.join("repo"))
                    .unwrap()
                    .next()
                    .is_none(),
                "retargeted anchor stays untouched for {mode:?}"
            );
            alias_lock.acquire(None).unwrap().release().unwrap();
        }
    }
}
