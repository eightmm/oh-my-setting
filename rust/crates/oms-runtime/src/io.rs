//! Retained pathname and generation proofs for runtime appends and selected outputs.
//! State paths enter with a protected repository root; explicit outputs enter with
//! their parsed absolute destination. Neither operation grants the other authority.
use crate::storage::{MAX_ROW, MAX_STREAM, Runtime, random_id};
use crate::{Result, error, require};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

fn io(e: impl std::fmt::Display) -> crate::Error {
    error(format!("runtime storage: {e}"))
}
fn changed() -> crate::Error {
    error("runtime pathname or generation changed; reconcile before retry")
}
fn identity(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn stable(a: &Stat, b: &Stat) -> bool {
    identity(a, b)
        && a.st_size == b.st_size
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_gid == b.st_gid
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}
fn regular(s: &Stat) -> bool {
    FileType::from_raw_mode(s.st_mode) == FileType::RegularFile && s.st_nlink == 1
}
fn trusted(rt: &Runtime<'_>, s: &Stat, directory: bool) -> Result<()> {
    let uid = rustix::process::getuid().as_raw();
    require(
        s.st_uid == uid
            && s.st_mode & 0o002 == 0
            && if directory {
                FileType::from_raw_mode(s.st_mode) == FileType::Directory
                    && s.st_mode & 0o500 == 0o500
            } else {
                regular(s) && s.st_mode & 0o400 != 0
            },
        "runtime record ownership, permissions or links are unsafe",
    )?;
    if s.st_mode & 0o020 != 0 {
        let resolve = rt
            .services
            .group_resolver()
            .ok_or_else(|| error("native private-group account proof service unavailable"))?;
        let group = resolve(s.st_gid)?;
        require(
            group.gid == s.st_gid
                && group.explicit_member_uids.iter().all(|v| *v == uid)
                && group.primary_member_uids == [uid],
            "runtime record group is not private",
        )?;
    }
    Ok(())
}
fn sync_dir(fd: &OwnedFd) -> Result<()> {
    match fs::fsync(fd) {
        Ok(()) | Err(Errno::BADF | Errno::INVAL | Errno::NOTSUP) => Ok(()),
        Err(e) => Err(io(e)),
    }
}
struct Directory {
    fd: OwnedFd,
    parent: Option<usize>,
    name: OsString,
    initial: Stat,
    protected: bool,
}
struct Anchor {
    dirs: Vec<Directory>,
    parent: usize,
    name: OsString,
    absolute: PathBuf,
    aliases: Vec<(PathBuf, Stat)>,
}
impl Anchor {
    fn open(rt: &Runtime<'_>, path: &Path, protected_root: Option<&Path>) -> Result<Self> {
        let raw = path.as_os_str().as_bytes();
        require(
            path.is_absolute()
                && !raw.contains(&0)
                && !raw.ends_with(b"/")
                && !raw.ends_with(b"/."),
            "runtime destination must name an absolute file without NUL or directory syntax",
        )?;
        let Some(Component::Normal(name)) = path.components().next_back() else {
            return Err(error("runtime destination requires a file name"));
        };
        let mut absolute = PathBuf::from("/");
        for component in path.components().skip(1) {
            match component {
                Component::Normal(part) => absolute.push(part),
                Component::CurDir => (),
                Component::ParentDir if protected_root.is_none() => {
                    // Match abspath before inspecting the immediate parent;
                    // resolving first could conceal a symlink behind `..`.
                    absolute.pop();
                }
                _ => return Err(error("invalid runtime destination component")),
            }
        }
        #[allow(unused_mut)]
        let mut aliases = vec![];
        // macOS system aliases are part of the supported host spelling. Capture
        // their exact identity; protected descendants never follow symbolic links.
        #[cfg(target_os = "macos")]
        for (logical, physical) in [("/tmp", "/private/tmp"), ("/var", "/private/var")] {
            if let Ok(relative) = absolute.strip_prefix(logical) {
                let info = fs::statat(fs::CWD, logical, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
                require(
                    std::fs::canonicalize(logical).map_err(io)? == Path::new(physical),
                    "unexpected system path alias",
                )?;
                aliases.push((PathBuf::from(logical), info));
                absolute = Path::new(physical).join(relative);
                break;
            }
        }
        let mut external_state_root = None;
        if protected_root.is_none() {
            let parent = absolute
                .parent()
                .ok_or_else(|| error("destination has no parent"))?;
            match std::fs::symlink_metadata(parent) {
                Ok(info) => require(
                    info.is_dir() && !info.file_type().is_symlink(),
                    "output directory must be a real directory",
                )?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(io(e)),
            }
            // Like _ensure_state_directory_chain, retain the last .oms suffix
            // for the no-follow walker rather than resolving its state links.
            if let Some(state) = parent
                .ancestors()
                .find(|part| part.file_name() == Some(OsStr::new(".oms")))
            {
                let before = state
                    .parent()
                    .expect("absolute state directory has a parent");
                let resolved = resolve_existing(before)?;
                external_state_root = Some(resolved.join(".oms"));
                absolute = resolved.join(
                    absolute
                        .strip_prefix(before)
                        .expect("state ancestor prefixes destination"),
                );
            } else {
                // Keep the final name unresolved so destination links refuse.
                absolute = resolve_existing(parent)?.join(name);
            }
        }
        let fd = fs::open("/", directory_flags(), Mode::empty()).map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        let mut a = Self {
            dirs: vec![Directory {
                fd,
                parent: None,
                name: "/".into(),
                initial,
                protected: false,
            }],
            parent: 0,
            name: name.to_owned(),
            absolute: absolute.clone(),
            aliases,
        };
        let parent = absolute
            .parent()
            .ok_or_else(|| error("destination has no parent"))?;
        let mut prefix = PathBuf::from("/");
        let mut protected = false;
        for component in parent.components().skip(1) {
            let name = match component {
                Component::Normal(name) => name,
                Component::CurDir => continue,
                Component::ParentDir => OsStr::new(".."),
                _ => return Err(error("invalid runtime destination component")),
            };
            prefix.push(name);
            protected |= protected_root
                .or(external_state_root.as_deref())
                .is_some_and(|root| prefix == root);
            a.check(rt)?;
            let observed = match fs::statat(a.fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(v) => v,
                Err(Errno::NOENT) => {
                    match fs::mkdirat(a.fd(), name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => sync_dir(a.fd())?,
                        Err(Errno::EXIST) => (),
                        Err(e) => return Err(io(e)),
                    }
                    fs::statat(a.fd(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?
                }
                Err(e) => return Err(io(e)),
            };
            require(
                FileType::from_raw_mode(observed.st_mode) == FileType::Directory,
                "runtime parent must be a real directory without links",
            )?;
            let fd = fs::openat(a.fd(), name, directory_flags(), Mode::empty()).map_err(io)?;
            let held = fs::fstat(&fd).map_err(io)?;
            require(
                identity(&held, &observed),
                "runtime parent changed during open",
            )?;
            if protected {
                trusted(rt, &held, true)?;
            }
            a.dirs.push(Directory {
                fd,
                parent: Some(a.parent),
                name: name.to_owned(),
                initial: held,
                protected,
            });
            a.parent = a.dirs.len() - 1;
        }
        a.check(rt)?;
        Ok(a)
    }
    fn fd(&self) -> &OwnedFd {
        &self.dirs[self.parent].fd
    }
    fn check(&self, rt: &Runtime<'_>) -> Result<()> {
        for (path, initial) in &self.aliases {
            let now =
                fs::statat(fs::CWD, path, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| changed())?;
            require(stable(initial, &now), "system path alias changed")?;
        }
        for d in &self.dirs {
            let named = match d.parent {
                Some(p) => fs::statat(&self.dirs[p].fd, &d.name, AtFlags::SYMLINK_NOFOLLOW),
                None => fs::stat("/"),
            }
            .map_err(|_| changed())?;
            let held = fs::fstat(&d.fd).map_err(|_| changed())?;
            require(
                identity(&named, &d.initial)
                    && identity(&held, &d.initial)
                    && FileType::from_raw_mode(named.st_mode) == FileType::Directory
                    && named.st_mode == held.st_mode
                    && named.st_uid == held.st_uid
                    && named.st_gid == held.st_gid,
                "runtime directory namespace changed",
            )?;
            if d.protected {
                trusted(rt, &held, true)?;
            }
        }
        Ok(())
    }
    fn leaf(&self, rt: &Runtime<'_>) -> Result<Option<Stat>> {
        self.check(rt)?;
        match fs::statat(self.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => Ok(Some(s)),
            Err(Errno::NOENT) => {
                self.check(rt)?;
                match fs::statat(self.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW) {
                    Err(Errno::NOENT) => Ok(None),
                    _ => Err(changed()),
                }
            }
            Err(e) => Err(io(e)),
        }
    }
    fn verify(&self, rt: &Runtime<'_>, fd: &OwnedFd, expected: &Stat) -> Result<()> {
        let held = fs::fstat(fd).map_err(io)?;
        let named = self.leaf(rt)?.ok_or_else(changed)?;
        trusted(rt, &held, false)?;
        require(
            stable(expected, &held) && stable(&held, &named),
            "runtime file generation changed",
        )?;
        self.check(rt)
    }
}
fn uncertain(e: impl std::fmt::Display) -> crate::Error {
    error(format!(
        "runtime append may be partial or durability uncertain; prior rows retained; reconcile before retry: {e}"
    ))
}
/// Requires the canonical stream lock. Reads zero history bytes and at most one
/// EOF byte. Captures the current generation under that lock rather than using a
/// command's earlier read snapshot. One O_APPEND write publishes the encoded row.
pub(crate) fn append(
    rt: &Runtime<'_>,
    path: &Path,
    root: &Path,
    row: &[u8],
    guard: &oms_lock::Guard,
) -> Result<()> {
    require(
        !row.is_empty()
            && row.len() <= MAX_ROW
            && row.ends_with(b"\n")
            && !row[..row.len() - 1].contains(&b'\n')
            && !row.contains(&0),
        "invalid runtime append row",
    )?;
    guard.check().map_err(io)?;
    let a = Anchor::open(rt, path, Some(root))?;
    let named = a.leaf(rt)?;
    if let Some(s) = &named {
        trusted(rt, s, false)?;
        require(
            s.st_mode & 0o222 != 0,
            "runtime append target has no write bits",
        )?;
    }
    let mut flags = OFlags::RDWR
        | OFlags::APPEND
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
        | OFlags::NOCTTY;
    if named.is_none() {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let fd = fs::openat(a.fd(), &a.name, flags, Mode::from_raw_mode(0o600)).map_err(io)?;
    if named.is_none() {
        fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(io)?;
    }
    let before = fs::fstat(&fd).map_err(io)?;
    trusted(rt, &before, false)?;
    require(
        named.as_ref().is_none_or(|s| stable(s, &before)),
        "runtime append target changed during open",
    )?;
    require(
        before.st_size >= 0 && before.st_size as u64 <= MAX_STREAM as u64,
        "append-only state exceeds the byte limit",
    )?;
    if before.st_size > 0 {
        let mut byte = [0u8];
        let count = rustix::io::pread(&fd, &mut byte, (before.st_size - 1) as u64).map_err(io)?;
        require(
            count == 1 && byte[0] == b'\n',
            "append-only JSONL has a partial final row",
        )?;
    }
    a.verify(rt, &fd, &before)?;
    rt.check()?;
    let expected = before
        .st_size
        .checked_add(row.len() as i64)
        .ok_or_else(|| error("append size overflow"))?;
    guard.check().map_err(io)?;
    let count = rustix::io::write(&fd, row).map_err(uncertain)?;
    if count != row.len() {
        return Err(uncertain(
            "short append; partial row requires reconciliation",
        ));
    }
    fs::fsync(&fd).map_err(uncertain)?;
    sync_dir(a.fd()).map_err(uncertain)?;
    let after = fs::fstat(&fd).map_err(uncertain)?;
    require(
        identity(&before, &after)
            && after.st_size == expected
            && before.st_mode == after.st_mode
            && before.st_uid == after.st_uid
            && before.st_gid == after.st_gid,
        "runtime append generation or size changed; reconcile before retry",
    )?;
    a.verify(rt, &fd, &after).map_err(uncertain)?;
    guard.check().map_err(uncertain)
}
struct Temporary<'a> {
    anchor: &'a Anchor,
    name: OsString,
    initial: Stat,
    published: bool,
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if let Ok(s) = fs::statat(self.anchor.fd(), &self.name, AtFlags::SYMLINK_NOFOLLOW) {
            if identity(&s, &self.initial) && regular(&s) {
                let _ = fs::unlinkat(self.anchor.fd(), &self.name, AtFlags::empty());
            }
        }
    }
}
/// Explicit selected destination only. Existing ancestor modes are never changed.
pub(crate) fn output(rt: &Runtime<'_>, path: &Path, body: &[u8]) -> Result<()> {
    replace(rt, path, body, None, None, None)
}
pub(crate) fn state(
    rt: &Runtime<'_>,
    path: &Path,
    body: &[u8],
    root: &Path,
    guard: &oms_lock::Guard,
    expected: Option<Option<crate::value::Value>>,
) -> Result<()> {
    replace(rt, path, body, Some(root), Some(guard), expected)
}
fn replace(
    rt: &Runtime<'_>,
    path: &Path,
    body: &[u8],
    root: Option<&Path>,
    guard: Option<&oms_lock::Guard>,
    expected: Option<Option<crate::value::Value>>,
) -> Result<()> {
    let a = Anchor::open(rt, path, root)?;
    let owned = if guard.is_none() {
        Some(lock(rt, &a.absolute)?)
    } else {
        None
    };
    let lock = guard.or(owned.as_ref()).expect("runtime publication lock");
    lock.check().map_err(io)?;
    a.check(rt)?;
    let before = a.leaf(rt)?;
    if let Some(expected) = expected {
        let actual = before.as_ref().map(oms_command_plan::storage::generation);
        require(
            actual == expected,
            "runtime source generation changed before publication",
        )?;
    }
    if let Some(s) = &before {
        trusted(rt, s, false)?;
        require(
            s.st_mode & 0o222 != 0,
            "runtime output target has no write bits",
        )?;
    }
    let mut name = OsString::from(".");
    name.push(&a.name);
    name.push(".");
    name.push(random_id("runtime-")?);
    let fd = fs::openat(
        a.fd(),
        &name,
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
    let mut temporary = Temporary {
        anchor: &a,
        name,
        initial,
        published: false,
    };
    fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(io)?;
    let mut remaining = body;
    while !remaining.is_empty() {
        let n = rustix::io::write(&fd, remaining).map_err(io)?;
        require(n > 0, "runtime output write returned zero")?;
        remaining = &remaining[n..];
    }
    fs::fsync(&fd).map_err(io)?;
    trusted(rt, &fs::fstat(&fd).map_err(io)?, false)?;
    lock.check().map_err(io)?;
    a.check(rt)?;
    let current = a.leaf(rt)?;
    require(
        match (&before, &current) {
            (None, None) => true,
            (Some(b), Some(c)) => stable(b, c),
            _ => false,
        },
        "runtime output changed before publication",
    )?;
    let held = fs::fstat(&fd).map_err(io)?;
    let named = fs::statat(a.fd(), &temporary.name, AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
    require(
        stable(&held, &named) && identity(&held, &temporary.initial),
        "runtime temporary changed before publication",
    )?;
    fs::renameat(a.fd(), &temporary.name, a.fd(), &a.name).map_err(|e| {
        error(format!(
            "runtime output publication uncertain; reconcile before retry: {e}"
        ))
    })?;
    temporary.published = true;
    sync_dir(a.fd()).map_err(|e| {
        error(format!(
            "runtime output published; durability uncertain: {e}"
        ))
    })?;
    let after = fs::fstat(&fd).map_err(|e| {
        error(format!(
            "runtime output published; metadata unavailable: {e}"
        ))
    })?;
    // Rename may advance ctime. Preserve identity, data and trust metadata, then
    // bind the published name to the newly captured generation.
    require(
        identity(&held, &after)
            && held.st_size == after.st_size
            && held.st_mode == after.st_mode
            && held.st_uid == after.st_uid
            && held.st_gid == after.st_gid
            && held.st_nlink == after.st_nlink
            && held.st_mtime == after.st_mtime
            && held.st_mtime_nsec == after.st_mtime_nsec,
        "runtime output published; generation changed; reconcile before retry",
    )?;
    a.verify(rt, &fd, &after).map_err(|e| {
        error(format!(
            "runtime output published; namespace proof failed: {e}"
        ))
    })?;
    lock.check().map_err(|e| {
        error(format!(
            "runtime output published; lock authority uncertain: {e}"
        ))
    })?;
    if let Some(owned) = owned {
        owned.release().map_err(|e| {
            error(format!(
                "runtime output published; lock release uncertain: {e}"
            ))
        })?;
    }
    Ok(())
}

fn resolve_existing(path: &Path) -> Result<PathBuf> {
    let mut cursor = path.to_path_buf();
    let mut missing = vec![];
    loop {
        match std::fs::canonicalize(&cursor) {
            Ok(mut path) => {
                for name in missing.iter().rev() {
                    path.push(name)
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    cursor
                        .file_name()
                        .ok_or_else(|| error("invalid runtime lock target"))?
                        .to_owned(),
                );
                require(cursor.pop(), "runtime lock target has no existing ancestor")?;
            }
            Err(e) => return Err(io(e)),
        }
    }
}

/// common.file_lock uses a runtime-SHA256 namespace and the canonical mkdir
/// protocol even on hosts with flock. Keep that authority across both writers.
pub(crate) fn lock(rt: &Runtime<'_>, target: &Path) -> Result<oms_lock::Guard> {
    let resolved = resolve_existing(target)?;
    let mut settings = oms_lock::Settings::from_env().map_err(io)?;
    settings.mode = oms_lock::Mode::Mkdir;
    settings.timeout = std::time::Duration::from_secs(15);
    settings.cwd = rt.cwd.clone();
    let root = if let Some(raw) = std::env::var_os("OMS_LOCK_DIR") {
        let raw = raw.as_bytes();
        let expanded = if raw == b"~" || raw.starts_with(b"~/") {
            settings
                .home
                .join(OsStr::from_bytes(raw.get(2..).unwrap_or(&[])))
        } else {
            PathBuf::from(OsStr::from_bytes(raw))
        };
        if expanded.is_absolute() {
            expanded
        } else {
            rt.cwd.join(expanded)
        }
    } else {
        settings.home.join(".cache/oh-my-setting/locks")
    };
    let stamp = crate::value::sha(
        resolved
            .to_str()
            .ok_or_else(|| error("runtime lock target cannot be encoded as UTF-8"))?,
    );
    let path = root.join(format!("runtime-{}.lock", &stamp[..32]));
    oms_lock::Lock::at_path(path, settings)
        .map_err(io)?
        .acquire(None)
        .map_err(|e| crate::status(e.exit_code(), format!("runtime state lock: {e}")))
}
