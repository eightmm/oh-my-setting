//! Descriptor retention for append and derived-index publication. Callers own
//! the canonical journal lock; metadata comparisons detect observed races only.
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

fn identity(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn generation(a: &Stat, b: &Stat) -> bool {
    identity(a, b)
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_nlink == b.st_nlink
}
fn regular(s: &Stat) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile && s.st_nlink == 1
}
fn owned(s: &Stat) -> bool {
    s.st_uid == rustix::process::geteuid().as_raw() && s.st_mode & 0o002 == 0
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn error(e: impl std::fmt::Debug) -> String {
    format!("journal namespace unavailable or changed: {e:?}")
}

pub(crate) struct Parent {
    root_path: PathBuf,
    root: OwnedFd,
    initial: Stat,
    names: Vec<OsString>,
    dirs: Vec<OwnedFd>,
    leaf: OsString,
}
impl Parent {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        Self::open_with(path, false)
    }
    fn open_with(path: &Path, create: bool) -> Result<Self, String> {
        let (safe_root, relative) = crate::schema::storage_root(path)?;
        let root_path = safe_root.physical_root().to_path_buf();
        let root = fs::open(&root_path, flags(), Mode::empty()).map_err(error)?;
        let initial = fs::fstat(&root).map_err(error)?;
        let mut names: Vec<_> = relative
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let leaf = names.pop().ok_or("invalid journal leaf")?;
        let mut result = Self {
            root_path,
            root,
            initial,
            names,
            dirs: Vec::new(),
            leaf,
        };
        for name in &result.names {
            if name == OsStr::new("..") {
                return Err("invalid journal ancestry".into());
            }
            result.validate()?;
            let named = match fs::statat(result.fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(named) => named,
                Err(rustix::io::Errno::NOENT) if create => {
                    match fs::mkdirat(result.fd(), name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => fs::fsync(result.fd()).map_err(error)?,
                        Err(rustix::io::Errno::EXIST) => {}
                        Err(e) => return Err(error(e)),
                    }
                    fs::statat(result.fd(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?
                }
                Err(e) => return Err(error(e)),
            };
            let dir = fs::openat(result.fd(), name, flags(), Mode::empty()).map_err(error)?;
            let stat = fs::fstat(&dir).map_err(error)?;
            if !owned(&stat) || !identity(&named, &stat) {
                return Err("unsafe journal directory owner or mode".into());
            }
            result.dirs.push(dir);
        }
        result.validate()?;
        Ok(result)
    }
    pub(crate) fn fd(&self) -> impl AsFd + '_ {
        self.dirs.last().unwrap_or(&self.root)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let named = fs::statat(rustix::fs::CWD, &self.root_path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(error)?;
        if !identity(&named, &self.initial) {
            return Err("journal root changed".into());
        }
        let mut parent = &self.root;
        for (name, dir) in self.names.iter().zip(&self.dirs) {
            let named = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
            let opened = fs::fstat(dir).map_err(error)?;
            if !identity(&named, &opened)
                || FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || !owned(&named)
            {
                return Err("journal ancestor changed".into());
            }
            parent = dir;
        }
        Ok(())
    }
    pub(crate) fn stat(&self) -> Result<Option<Stat>, String> {
        self.validate()?;
        match fs::statat(self.fd(), &self.leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if regular(&stat) && owned(&stat) => Ok(Some(stat)),
            Ok(_) => Err("unsafe journal file owner, mode or type".into()),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }
    pub(crate) fn sync(&self) -> Result<(), String> {
        self.validate()?;
        fs::fsync(self.fd()).map_err(error)
    }
}

pub(crate) fn append_event(path: &Path, row: &[u8]) -> Result<(), String> {
    let parent = Parent::open(path)?;
    let before = parent.stat()?;
    let options =
        OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let options = if before.is_none() {
        options | OFlags::CREATE | OFlags::EXCL
    } else {
        options
    };
    let descriptor = fs::openat(
        parent.fd(),
        &parent.leaf,
        options,
        Mode::from_raw_mode(0o600),
    )
    .map_err(error)?;
    let opened = fs::fstat(&descriptor).map_err(error)?;
    if !regular(&opened)
        || !owned(&opened)
        || before.as_ref().is_some_and(|s| !generation(s, &opened))
    {
        return Err("journal changed during append open".into());
    }
    parent.validate()?;
    let named = parent.stat()?.ok_or("journal disappeared before append")?;
    if !generation(&opened, &named) {
        return Err("journal changed before append".into());
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(row.len() + 2);
    if opened.st_size > 0 {
        file.seek(SeekFrom::End(-1)).map_err(error)?;
        let mut last = [0];
        file.read_exact(&mut last).map_err(error)?;
        if last[0] != b'\n' {
            bytes.push(b'\n');
        }
    }
    bytes.extend_from_slice(row);
    bytes.push(b'\n');
    // No rollback: an error after the first write may have admitted a prefix,
    // or a complete row whose durability has not yet been established.
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            format!("journal append may be partial or durable; reconcile before retry: {e}")
        })?;
    let after = fs::fstat(&file).map_err(error)?;
    let named = parent
        .stat()?
        .ok_or("journal appended to detached inode; durability uncertain")?;
    if !generation(&after, &named)
        || !identity(&opened, &after)
        || after.st_size != opened.st_size + i64::try_from(bytes.len()).map_err(error)?
    {
        return Err("journal append namespace or length changed; publication uncertain".into());
    }
    parent
        .sync()
        .map_err(|e| format!("journal append visible; directory durability uncertain: {e}"))
}

pub(crate) struct PrivateIndex {
    parent: Parent,
    name: OsString,
    dir: OwnedFd,
    initial: Stat,
    file: File,
    previous: Option<Stat>,
    path: PathBuf,
    published: bool,
}
impl PrivateIndex {
    pub(crate) fn new(destination: &Path) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = Parent::open(destination)?;
        let previous = parent.stat()?;
        for _ in 0..128 {
            let name = OsString::from(format!(
                ".index-private-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            parent.validate()?;
            match fs::mkdirat(parent.fd(), &name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => continue,
                Err(e) => return Err(error(e)),
            }
            let dir = fs::openat(parent.fd(), &name, flags(), Mode::empty()).map_err(error)?;
            let initial = fs::fstat(&dir).map_err(error)?;
            if !owned(&initial) || initial.st_mode & 0o777 != 0o700 {
                return Err("unsafe private index directory".into());
            }
            let fd = fs::openat(
                &dir,
                "index",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(error)?;
            let mut path = parent.root_path.clone();
            for part in &parent.names {
                path.push(part);
            }
            path.push(&name);
            path.push("index");
            let result = Self {
                parent,
                name,
                dir,
                initial,
                file: File::from(fd),
                previous,
                path,
                published: false,
            };
            result.validate()?;
            return Ok(result);
        }
        Err("private index directory collision limit".into())
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.parent.validate()?;
        let named =
            fs::statat(self.parent.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
        if !identity(&named, &self.initial) || !owned(&named) || named.st_mode & 0o777 != 0o700 {
            return Err("private index directory changed".into());
        }
        let named = fs::statat(&self.dir, "index", AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
        let opened = fs::fstat(&self.file).map_err(error)?;
        if !regular(&named) || !owned(&named) || !identity(&named, &opened) {
            return Err("private index file changed".into());
        }
        Ok(())
    }
    pub(crate) fn install(mut self) -> Result<(), String> {
        self.validate()?;
        match (self.previous.as_ref(), self.parent.stat()?) {
            (None, None) => {}
            (Some(before), Some(after)) if generation(before, &after) => {}
            _ => return Err("derived index destination changed".into()),
        }
        self.file.sync_all().map_err(error)?;
        fs::renameat(&self.dir, "index", self.parent.fd(), &self.parent.leaf).map_err(|e| {
            format!("derived index publication uncertain; reconcile before retry: {e}")
        })?;
        self.published = true;
        fs::fsync(&self.dir)
            .and_then(|()| fs::fsync(self.parent.fd()))
            .map_err(|e| format!("derived index visible; directory durability uncertain: {e}"))?;
        self.parent.validate()?;
        let named = self
            .parent
            .stat()?
            .ok_or("derived index disappeared after publication")?;
        if !identity(&named, &fs::fstat(&self.file).map_err(error)?) {
            return Err("derived index replaced after publication".into());
        }
        Ok(())
    }
}
impl Drop for PrivateIndex {
    fn drop(&mut self) {
        if self.parent.validate().is_err() {
            return;
        }
        let Ok(named) = fs::statat(self.parent.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        if !identity(&named, &self.initial) {
            return;
        }
        if !self.published {
            let Ok(named) = fs::statat(&self.dir, "index", AtFlags::SYMLINK_NOFOLLOW) else {
                return;
            };
            let Ok(opened) = fs::fstat(&self.file) else {
                return;
            };
            if !regular(&named) || !identity(&named, &opened) {
                return;
            }
            let _ = fs::unlinkat(&self.dir, "index", AtFlags::empty());
        }
        // Never recurse into leftovers or remove names we did not create.
        let _ = fs::unlinkat(self.parent.fd(), &self.name, AtFlags::REMOVEDIR);
    }
}

pub(crate) fn remove_owned_file(path: &Path) -> Result<(), String> {
    let parent = match Parent::open(path) {
        Ok(parent) => parent,
        Err(error) => {
            if crate::cold::read_regular_no_symlink_bytes(path)?.is_none() {
                return Ok(());
            }
            return Err(error);
        }
    };
    let Some(before) = parent.stat()? else {
        return Ok(());
    };
    let opened = fs::openat(
        parent.fd(),
        &parent.leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    if !generation(&before, &fs::fstat(&opened).map_err(error)?) {
        return Err("journal file changed before removal".into());
    }
    parent.validate()?;
    if !parent
        .stat()?
        .is_some_and(|named| generation(&before, &named))
    {
        return Err("journal removal namespace changed".into());
    }
    fs::unlinkat(parent.fd(), &parent.leaf, AtFlags::empty())
        .map_err(|e| format!("journal removal outcome uncertain: {e}"))?;
    parent
        .sync()
        .map_err(|e| format!("journal file removed; directory durability uncertain: {e}"))
}

struct Replacement<'a> {
    parent: &'a Parent,
    name: OsString,
    file: File,
    published: bool,
}
impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        if self.published || self.parent.validate().is_err() {
            return;
        }
        let Ok(named) = fs::statat(self.parent.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        let Ok(opened) = fs::fstat(&self.file) else {
            return;
        };
        if regular(&named) && owned(&named) && identity(&named, &opened) {
            let _ = fs::unlinkat(self.parent.fd(), &self.name, AtFlags::empty());
        }
    }
}
pub(crate) fn replace_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = Parent::open_with(path, true)?;
    let before = parent.stat()?;
    let mut replacement = None;
    for _ in 0..128 {
        let name = OsString::from(format!(
            ".journal-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::openat(
            parent.fd(),
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => {
                replacement = Some(Replacement {
                    parent: &parent,
                    name,
                    file: File::from(fd),
                    published: false,
                });
                break;
            }
            Err(rustix::io::Errno::EXIST) => continue,
            Err(e) => return Err(error(e)),
        }
    }
    let mut replacement = replacement.ok_or("journal temporary collision limit")?;
    let opened = fs::fstat(&replacement.file).map_err(error)?;
    if !regular(&opened) || !owned(&opened) || opened.st_mode & 0o777 != 0o600 {
        return Err("unsafe journal temporary file".into());
    }
    replacement
        .file
        .write_all(content)
        .and_then(|()| replacement.file.sync_all())
        .map_err(|e| format!("journal replacement body was not published: {e}"))?;
    parent.validate()?;
    match (before, parent.stat()?) {
        (None, None) => {}
        (Some(before), Some(after)) if generation(&before, &after) => {}
        _ => return Err("journal destination changed before replacement".into()),
    }
    let named =
        fs::statat(parent.fd(), &replacement.name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
    let body = fs::fstat(&replacement.file).map_err(error)?;
    if !regular(&named) || !owned(&named) || !generation(&named, &body) {
        return Err("journal temporary changed before publication".into());
    }
    fs::renameat(parent.fd(), &replacement.name, parent.fd(), &parent.leaf).map_err(|e| {
        format!("journal replacement publication uncertain; reconcile before retry: {e}")
    })?;
    replacement.published = true;
    parent
        .sync()
        .map_err(|e| format!("journal replacement visible; directory durability uncertain: {e}"))?;
    let named = parent
        .stat()?
        .ok_or("journal replacement disappeared after publication")?;
    if !identity(&named, &body) {
        return Err("journal replacement changed after publication; durability uncertain".into());
    }
    Ok(())
}

pub(crate) fn ensure_directory(path: &Path) -> Result<(), String> {
    Parent::open_with(&path.join("entry"), true)?.sync()
}

pub(crate) fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let parent = Parent::open(path)?;
    let before = fs::statat(parent.fd(), &parent.leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
    if !regular(&before) {
        return Err("source requires a regular file".into());
    }
    let descriptor = fs::openat(
        parent.fd(),
        &parent.leaf,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let opened = fs::fstat(&descriptor).map_err(error)?;
    if !generation(&before, &opened) {
        return Err("source changed during prefix open".into());
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    let after = fs::fstat(&file).map_err(error)?;
    parent.validate()?;
    let named = fs::statat(parent.fd(), &parent.leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
    if !generation(&opened, &after) || !generation(&after, &named) {
        return Err("source changed during prefix read".into());
    }
    Ok(bytes)
}
