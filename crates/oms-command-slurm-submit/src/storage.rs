//! Descriptor-relative append publication; never replace the authoritative JSONL inode.
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Component, Path, PathBuf};

pub(crate) type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "unavailable, changed, or unsafe submission ledger";
fn dirs() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn owned(s: &Stat) -> bool {
    s.st_uid == rustix::process::geteuid().as_raw() && s.st_mode & 0o002 == 0
}
fn regular(s: &Stat) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile && s.st_nlink == 1 && owned(s)
}
fn sync(fd: &OwnedFd) -> Result<()> {
    fs::fsync(fd).map_err(|_| INVALID)
}

/// Reject invalid leaf spellings before selecting or creating a lock namespace.
pub(crate) fn validate_path(path: &Path) -> Result<()> {
    let raw = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || raw.contains(&0)
        || raw.ends_with(b"/")
        || raw.ends_with(b"/.")
        || !matches!(path.components().next_back(), Some(Component::Normal(_)))
    {
        return Err(INVALID);
    }
    Ok(())
}

struct Alias {
    logical: PathBuf,
    physical: PathBuf,
    directory: Stat,
    system_link: Option<Stat>,
}
impl Alias {
    fn open(logical: &Path) -> Result<Self> {
        if !logical.is_absolute() {
            return Err(INVALID);
        }
        let physical = std::fs::canonicalize(logical).map_err(|_| INVALID)?;
        let directory =
            fs::statat(fs::CWD, &physical, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| INVALID)?;
        if FileType::from_raw_mode(directory.st_mode) != FileType::Directory {
            return Err(INVALID);
        }
        let alias = Self {
            logical: logical.to_owned(),
            physical,
            directory,
            system_link: None,
        };
        alias.check()?;
        Ok(alias)
    }
    fn check(&self) -> Result<()> {
        if std::fs::canonicalize(&self.logical).map_err(|_| INVALID)? != self.physical {
            return Err(INVALID);
        }
        let current = fs::statat(fs::CWD, &self.logical, AtFlags::empty()).map_err(|_| INVALID)?;
        if !same(&current, &self.directory)
            || current.st_uid != self.directory.st_uid
            || FileType::from_raw_mode(current.st_mode) != FileType::Directory
        {
            return Err(INVALID);
        }
        if let Some(link) = &self.system_link {
            let current = fs::statat(fs::CWD, &self.logical, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| INVALID)?;
            if !same(link, &current)
                || current.st_uid != 0
                || FileType::from_raw_mode(current.st_mode) != FileType::Symlink
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}

fn resolve(path: &Path, cwd: &Path) -> Result<(PathBuf, Vec<Alias>)> {
    validate_path(path)?;
    if let Ok(tail) = path.strip_prefix(cwd) {
        let root = Alias::open(cwd)?;
        if root.logical.as_os_str() != root.physical.as_os_str() {
            return Ok((root.physical.join(tail), vec![root]));
        }
    }
    // These root-owned platform aliases are not ledger authority. Resolve only
    // their documented destinations, retaining proof of the original alias.
    #[cfg(target_os = "macos")]
    for name in ["tmp", "var"] {
        let logical = Path::new("/").join(name);
        if let Ok(tail) = path.strip_prefix(&logical) {
            let link =
                fs::statat(fs::CWD, &logical, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| INVALID)?;
            if FileType::from_raw_mode(link.st_mode) == FileType::Symlink {
                if link.st_uid != 0 {
                    return Err(INVALID);
                }
                let mut alias = Alias::open(&logical)?;
                if alias.physical != Path::new("/private").join(name) {
                    return Err(INVALID);
                }
                alias.system_link = Some(link);
                alias.check()?;
                return Ok((alias.physical.join(tail), vec![alias]));
            }
        }
    }
    Ok((path.to_owned(), Vec::new()))
}

struct Anchor {
    names: Vec<OsString>,
    descriptors: Vec<OwnedFd>,
    aliases: Vec<Alias>,
}
impl Anchor {
    fn open(path: &Path, cwd: &Path, create: bool) -> Result<Option<Self>> {
        let (path, aliases) = resolve(path, cwd)?;
        let mut names = Vec::new();
        for c in path.components() {
            match c {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(n) => names.push(n.to_owned()),
                // Traverse rather than erase: a preceding symlink must be
                // refused, not bypassed by lexical `name/..` normalization.
                Component::ParentDir => names.push("..".into()),
                _ => return Err(INVALID),
            }
        }
        if names.is_empty() {
            return Err(INVALID);
        }
        let root = fs::open("/", dirs(), Mode::empty()).map_err(|_| INVALID)?;
        let mut anchor = Self {
            names,
            descriptors: vec![root],
            aliases,
        };
        for i in 0..anchor.names.len() - 1 {
            let parent = anchor.descriptors.last().unwrap();
            let name = &anchor.names[i];
            let before = match fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(s) => s,
                Err(Errno::NOENT) if create => {
                    match fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => sync(parent)?,
                        Err(Errno::EXIST) => {}
                        _ => return Err(INVALID),
                    }
                    fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| INVALID)?
                }
                Err(Errno::NOENT) => {
                    anchor.check()?;
                    return Ok(None);
                }
                _ => return Err(INVALID),
            };
            if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
                return Err(INVALID);
            }
            let fd = fs::openat(parent, name, dirs(), Mode::empty()).map_err(|_| INVALID)?;
            if !same(&before, &fs::fstat(&fd).map_err(|_| INVALID)?) {
                return Err(INVALID);
            }
            anchor.descriptors.push(fd);
        }
        // The final directory carries local write authority. A root-owned sticky
        // temporary directory is also valid for an explicitly selected file.
        let p = fs::fstat(anchor.parent()).map_err(|_| INVALID)?;
        if !owned(&p) && !(p.st_uid == 0 && p.st_mode & 0o1000 != 0) {
            return Err(INVALID);
        }
        anchor.check()?;
        Ok(Some(anchor))
    }
    fn parent(&self) -> &OwnedFd {
        self.descriptors.last().unwrap()
    }
    fn name(&self) -> &OsString {
        self.names.last().unwrap()
    }
    fn check(&self) -> Result<()> {
        for alias in &self.aliases {
            alias.check()?;
        }
        for i in 0..self.descriptors.len() - 1 {
            let named = fs::statat(
                &self.descriptors[i],
                &self.names[i],
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| INVALID)?;
            let held = fs::fstat(&self.descriptors[i + 1]).map_err(|_| INVALID)?;
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory || !same(&named, &held)
            {
                return Err(INVALID);
            }
        }
        if self.descriptors.len() == self.names.len() {
            let parent = fs::fstat(self.parent()).map_err(|_| INVALID)?;
            if !owned(&parent) && !(parent.st_uid == 0 && parent.st_mode & 0o1000 != 0) {
                return Err(INVALID);
            }
        }
        Ok(())
    }
    fn leaf(&self) -> Result<Option<Stat>> {
        match fs::statat(self.parent(), self.name(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) if regular(&s) => Ok(Some(s)),
            Err(Errno::NOENT) => {
                self.check()?;
                Ok(None)
            }
            _ => Err(INVALID),
        }
    }
    fn check_file(&self, file: &File) -> Result<Stat> {
        self.check()?;
        let held = fs::fstat(file).map_err(|_| INVALID)?;
        if !regular(&held) || !self.leaf()?.is_some_and(|s| same(&s, &held)) {
            return Err(INVALID);
        }
        Ok(held)
    }
}

pub(crate) fn load(path: &Path, cwd: &Path) -> Result<Vec<oms_context::Json>> {
    let Some(anchor) = Anchor::open(path, cwd, false)? else {
        return Ok(Vec::new());
    };
    let Some(before) = anchor.leaf()? else {
        return Ok(Vec::new());
    };
    let fd = fs::openat(
        anchor.parent(),
        anchor.name(),
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| INVALID)?;
    let file = File::from(fd);
    if !same(&before, &anchor.check_file(&file)?) {
        return Err(INVALID);
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut rows = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(|_| INVALID)? == 0 {
            break;
        }
        // Legacy reads UTF-8 with replacement and skips malformed/non-object rows.
        let decoded = String::from_utf8_lossy(&line);
        match oms_context::parse_experiment_json(decoded.as_bytes(), decoded.len()) {
            Ok(value) if matches!(value, oms_context::Json::Object(_)) => rows.push(value),
            Err("parser infrastructure failure") => return Err(INVALID),
            _ => {}
        }
    }
    let after = anchor.check_file(reader.get_ref())?;
    if before.st_size != after.st_size
        || before.st_mtime != after.st_mtime
        || before.st_mtime_nsec != after.st_mtime_nsec
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
    {
        return Err(INVALID);
    }
    Ok(rows)
}

pub(crate) fn append(path: &Path, cwd: &Path, bytes: &[u8]) -> Result<()> {
    let anchor = Anchor::open(path, cwd, true)?.ok_or(INVALID)?;
    let before = anchor.leaf()?;
    let flags =
        OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let fd = if before.is_some() {
        fs::openat(anchor.parent(), anchor.name(), flags, Mode::empty())
    } else {
        fs::openat(
            anchor.parent(),
            anchor.name(),
            flags | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(0o600),
        )
    }
    .map_err(|_| INVALID)?;
    let mut file = File::from(fd);
    let held = anchor.check_file(&file)?;
    if before.is_some_and(|s| !same(&s, &held)) {
        return Err(INVALID);
    }
    if before.is_none() {
        fs::fchmod(&file, Mode::from_raw_mode(0o600)).map_err(|_| INVALID)?;
    }
    // A killed append can leave an unterminated fragment. Separate the next
    // record so a later intent/result cannot disappear into that fragment.
    if held.st_size > 0 {
        let mut last = [0];
        if file
            .read_at(&mut last, held.st_size as u64 - 1)
            .map_err(|_| INVALID)?
            != 1
        {
            return Err(INVALID);
        }
        if last[0] != b'\n' {
            file.write_all(b"\n").map_err(|_| INVALID)?;
        }
    }
    // Caller holds the shared oms-lock until both file and directory are durable.
    file.write_all(bytes).map_err(|_| INVALID)?;
    file.sync_all().map_err(|_| INVALID)?;
    anchor.check_file(&file)?;
    sync(anchor.parent())
}

pub(crate) fn ensure_ignore(path: &Path, cwd: &Path, interrupted: &std::sync::atomic::AtomicBool) {
    match Anchor::open(path, cwd, false) {
        Err(_) => return,
        Ok(Some(anchor)) if anchor.leaf().is_err() => return,
        _ => {}
    }
    let mut prefix = PathBuf::new();
    let mut repo = None;
    for c in path.components() {
        if c.as_os_str() == ".oms" {
            repo = Some(prefix.clone());
        }
        prefix.push(c);
    }
    let Some(repo) = repo else {
        return;
    };
    let ignore = repo.join(".oms/.gitignore");
    // Use the shared no-follow atomic primitive for the ancillary ignore file;
    // unlike the ledger it has no append/inode compatibility requirement.
    if let Ok(_guard) = crate::ledger::acquire(&ignore, cwd, interrupted)
        && _guard.check().is_ok()
        && let Ok(root) = oms_safe_fs::Root::open(&repo)
    {
        let options = oms_safe_fs::MutationOptions {
            read: oms_safe_fs::ByteReadOptions {
                missing_ok: true,
                max_bytes: usize::MAX,
            },
            create_parent: true,
            max_output: 2,
            preserve_existing: true,
            ..Default::default()
        };
        let _ = root.atomic_mutate(Path::new(".oms/.gitignore"), options, |_| {
            Ok::<_, ()>(b"*\n".to_vec())
        });
    }
}
