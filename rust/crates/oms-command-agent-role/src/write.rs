use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::{
    fs::File,
    io::{self, Write},
    os::fd::OwnedFd,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) enum Failure {
    Exists,
    Interrupted,
    Io(io::Error),
}
impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rustix::io::Errno> for Failure {
    fn from(e: rustix::io::Errno) -> Self {
        Self::Io(e.into())
    }
}
fn check(interrupted: &AtomicBool) -> Result<(), Failure> {
    if interrupted.load(Ordering::Relaxed) {
        Err(Failure::Interrupted)
    } else {
        Ok(())
    }
}
fn directory(parent: &impl std::os::fd::AsFd, name: &str) -> Result<OwnedFd, Failure> {
    match fs::mkdirat(parent, name, Mode::from_raw_mode(0o777)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Pending<'a> {
    dir: &'a OwnedFd,
    name: String,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let _ = fs::unlinkat(self.dir, self.name.as_str(), AtFlags::empty());
    }
}
fn publish(
    dir: &OwnedFd,
    name: &str,
    body: &[u8],
    interrupted: &AtomicBool,
) -> Result<(), Failure> {
    check(interrupted)?;
    // A completed temporary inode is linked into the public name exclusively.
    // Competing calls cannot overwrite it; a kill leaves at most an unreferenced
    // hidden staging file, never a partial role or a replaced symlink target.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let staging = format!(
        ".oms-role-{}-{nonce}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let fd = fs::openat(
        dir,
        staging.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o666),
    )?;
    let pending = Pending { dir, name: staging };
    let mut file = File::from(fd);
    file.write_all(body)?;
    file.sync_all()?;
    check(interrupted)?;
    match fs::linkat(dir, pending.name.as_str(), dir, name, AtFlags::empty()) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::EXIST) => Err(Failure::Exists),
        Err(e) => Err(e.into()),
    }
}

fn global_directory(path: &Path) -> Result<OwnedFd, Failure> {
    // Anchor the existing parent physically, then create missing components via
    // directory descriptors. The role store itself is never followed as a link.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let parent_fd = if parent.is_dir() {
            fs::open(
                parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?
        } else {
            global_directory(parent)?
        };
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("invalid global roles directory"))?;
        match fs::mkdirat(&parent_fd, name, Mode::from_raw_mode(0o777)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(fs::openat(
            &parent_fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

pub(crate) fn init(
    dir: &Path,
    filename: &str,
    name: &str,
    project: bool,
    interrupted: &AtomicBool,
) -> Result<(), Failure> {
    check(interrupted)?;
    // lstat includes dangling links, which must never be used as write targets.
    match std::fs::symlink_metadata(dir.join(filename)) {
        Ok(_) => return Err(Failure::Exists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let roles = if project {
        let repo = dir
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("invalid project role path"))?;
        let root = fs::open(
            repo,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let oms = directory(&root, ".oms")?;
        // The ignore file is best-effort, like agent_memory_ensure_oms_ignore.
        let _ = publish(&oms, ".gitignore", b"*\n", interrupted);
        directory(&oms, "roles")?
    } else {
        global_directory(dir)?
    };
    let body = format!(
        "# Role: {name}\n\nYou are acting as the \"{name}\" role for this task.\n\n## Mandate\n\n- Describe what this role does and does not do.\n\n## Rules\n\n- One actionable rule per line.\n\n## Output\n\n- State what the role must produce.\n"
    );
    publish(&roles, filename, body.as_bytes(), interrupted)
}
