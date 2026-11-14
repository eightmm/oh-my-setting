//! Descriptor-relative private writes, with metadata as the commit marker.
use crate::{Result, UNSAFE, checkpoint, json};
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
struct Directory {
    fd: OwnedFd,
    path: PathBuf,
}
impl Directory {
    fn root(path: &Path) -> Result<Self> {
        let fd = open_directory(path)?;
        Ok(Self {
            fd,
            path: path.to_owned(),
        })
    }
    fn child(&self, name: &OsStr, create: bool) -> Result<Self> {
        self.open_child(name, create, true)
    }
    fn open_child(&self, name: &OsStr, create: bool, private_leaf: bool) -> Result<Self> {
        self.verify()?;
        let created = if create {
            match fs::mkdirat(&self.fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => true,
                Err(rustix::io::Errno::EXIST) => false,
                Err(_) => return Err(UNSAFE),
            }
        } else {
            false
        };
        let fd = fs::openat(&self.fd, name, DIR_FLAGS, Mode::empty()).map_err(|_| UNSAFE)?;
        let info = fs::fstat(&fd).map_err(|_| UNSAFE)?;
        if create && (created || private_leaf) {
            if info.st_uid != rustix::process::getuid().as_raw() {
                return Err(UNSAFE);
            }
            fs::fchmod(&fd, Mode::from_raw_mode(0o700)).map_err(|_| UNSAFE)?;
        }
        let dir = Self {
            fd,
            path: self.path.join(name),
        };
        dir.verify()?;
        Ok(dir)
    }
    fn verify(&self) -> Result<()> {
        let named = open_directory(&self.path)?;
        let current = fs::fstat(&named).map_err(|_| UNSAFE)?;
        let opened = fs::fstat(&self.fd).map_err(|_| UNSAFE)?;
        if current.st_dev != opened.st_dev
            || current.st_ino != opened.st_ino
            || FileType::from_raw_mode(current.st_mode) != FileType::Directory
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    fn write_new(&self, name: &str, bytes: &[u8], interrupted: &AtomicBool) -> Result<()> {
        self.verify()?;
        checkpoint(interrupted)?;
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| UNSAFE)?;
        fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(|_| UNSAFE)?;
        let mut file = File::from(fd);
        file.write_all(bytes).map_err(|_| UNSAFE)?;
        file.sync_all().map_err(|_| UNSAFE)?;
        self.verify()?;
        checkpoint(interrupted)?;
        Ok(())
    }
    fn exists(&self, name: &OsStr) -> Result<bool> {
        self.verify()?;
        match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(info) if FileType::from_raw_mode(info.st_mode) == FileType::RegularFile => Ok(true),
            Ok(_) => Err(UNSAFE),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(UNSAFE),
        }
    }
}
fn open_directory(path: &Path) -> Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(UNSAFE);
    }
    let mut fd = fs::open("/", DIR_FLAGS, Mode::empty()).map_err(|_| UNSAFE)?;
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                fd = fs::openat(&fd, name, DIR_FLAGS, Mode::empty()).map_err(|_| UNSAFE)?;
            }
            _ => return Err(UNSAFE),
        }
    }
    Ok(fd)
}

pub fn plain(path: &Path) -> Result<PathBuf> {
    let mut checked = PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir) {
            return Err(UNSAFE);
        }
        checked.push(part);
        match std::fs::symlink_metadata(&checked) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err("input paths must not traverse symbolic links");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(UNSAFE),
        }
    }
    Ok(checked)
}

/// Only trusted fixed Git subcommands are used; no source commands or transforms.
struct GitChild {
    child: Child,
    group: Pid,
}
impl Drop for GitChild {
    fn drop(&mut self) {
        // waitid(NOWAIT) below keeps this child waitable until group cleanup,
        // so its PID cannot be reused to name somebody else's process group.
        let _ = rustix::process::kill_process_group(self.group, Signal::KILL);
        let _ = self.child.wait();
    }
}
pub fn bounded_git(
    repo: &Path,
    args: &[&OsStr],
    limit: usize,
    interrupted: &AtomicBool,
) -> Result<Vec<u8>> {
    checkpoint(interrupted)?;
    let mut command = Command::new("git");
    command
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0");
    run_bounded(command, limit, interrupted, 5, UNSAFE, UNSAFE, UNSAFE)
}
/// Run one fixed read-only program in its own process group under byte and time bounds.
pub fn run_bounded(
    mut command: Command,
    limit: usize,
    interrupted: &AtomicBool,
    seconds: u64,
    unavailable: &'static str,
    oversize: &'static str,
    failed: &'static str,
) -> Result<Vec<u8>> {
    let child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unavailable)?;
    let group = Pid::from_raw(child.id() as i32).ok_or(UNSAFE)?;
    let mut owned = GitChild { child, group };
    let pipe = owned.child.stdout.take().ok_or(UNSAFE)?;
    let flags = fs::fcntl_getfl(&pipe).map_err(|_| UNSAFE)?;
    fs::fcntl_setfl(&pipe, flags | OFlags::NONBLOCK).map_err(|_| UNSAFE)?;
    let mut reader = pipe;
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let progress = || {
        checkpoint(interrupted)?;
        if Instant::now() >= deadline {
            Err("local capture timed out")
        } else {
            Ok(())
        }
    };
    loop {
        progress()?;
        let mut bytes = [0; 8192];
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                output.extend_from_slice(&bytes[..count]);
                if output.len() > limit {
                    return Err(oversize);
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(UNSAFE),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    loop {
        progress()?;
        match rustix::process::waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => {
                return if status.exit_status() == Some(0) {
                    Ok(output)
                } else {
                    Err(failed)
                };
            }
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(UNSAFE),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn inputs(repo: &Path, create: bool) -> Result<Directory> {
    Directory::root(repo)?
        // The shared ancestor may serve other OMS components; only a newly
        // created ancestor or the requested private leaf gets private modes.
        .open_child(OsStr::new(".oms"), create, false)?
        .child(OsStr::new("inputs"), create)
}
fn ensure_ignored(repo: &Path, interrupted: &AtomicBool) -> Result<Directory> {
    let inputs = inputs(repo, true)?;
    if !inputs.exists(OsStr::new(".gitignore"))? {
        let name = format!(".ignore-{}", random_id()?);
        let written = (|| {
            inputs.write_new(&name, b"*\n", interrupted)?;
            // linkat publishes without replacement; another writer may win.
            match fs::linkat(
                &inputs.fd,
                &name,
                &inputs.fd,
                ".gitignore",
                AtFlags::empty(),
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
                Err(_) => Err(UNSAFE),
            }
        })();
        let _ = fs::unlinkat(&inputs.fd, &name, AtFlags::empty());
        written?;
    }
    inputs.verify()?;
    fs::fsync(&inputs.fd).map_err(|_| UNSAFE)?;
    bounded_git(
        repo,
        &[
            OsStr::new("check-ignore"),
            OsStr::new("-q"),
            OsStr::new("--"),
            OsStr::new(".oms/inputs/queue.json"),
        ],
        1,
        interrupted,
    )
    .map_err(|_| "private inputs must be ignored by Git")?;
    Ok(inputs)
}
fn prepare_private(repo: &Path, interrupted: &AtomicBool) -> Result<(Directory, String)> {
    let inputs = ensure_ignored(repo, interrupted)?;
    let id = format!("attachment-{}", random_id()?);
    Ok((inputs, id))
}
pub fn create(
    repo: &Path,
    interrupted: &AtomicBool,
    record: impl FnOnce(&str) -> json::J,
    content: &[u8],
) -> Result<json::J> {
    create_with_hook(repo, interrupted, record, content, || {})
}
fn create_with_hook(
    repo: &Path,
    interrupted: &AtomicBool,
    record: impl FnOnce(&str) -> json::J,
    content: &[u8],
    after_content: impl FnOnce(),
) -> Result<json::J> {
    let (inputs, id) = prepare_private(repo, interrupted)?;
    for name in ["content.txt", ".meta.pending", "meta.json"] {
        let path = PathBuf::from(".oms/inputs/attachments")
            .join(&id)
            .join(name);
        bounded_git(
            repo,
            &[
                OsStr::new("check-ignore"),
                OsStr::new("-q"),
                OsStr::new("--"),
                path.as_os_str(),
            ],
            1,
            interrupted,
        )
        .map_err(|_| "private attachment snapshots must be ignored by Git")?;
    }
    checkpoint(interrupted)?;
    let attachments = inputs.child(OsStr::new("attachments"), true)?;
    fs::mkdirat(&attachments.fd, &id, Mode::from_raw_mode(0o700)).map_err(|_| UNSAFE)?;
    let folder = match attachments.child(OsStr::new(&id), false) {
        Ok(folder) => folder,
        Err(error) => {
            let _ = fs::unlinkat(&attachments.fd, &id, AtFlags::REMOVEDIR);
            return Err(error);
        }
    };
    let record = record(&id);
    let result: Result<()> = (|| {
        fs::fchmod(&folder.fd, Mode::from_raw_mode(0o700)).map_err(|_| UNSAFE)?;
        folder.write_new("content.txt", content, interrupted)?;
        after_content();
        let encoded = json::encode(&record)?;
        // meta.json appears only after its bytes have been fsynced. Readers
        // ignore a crash-left content-only directory, and IDs are never reused.
        folder.write_new(".meta.pending", &encoded, interrupted)?;
        folder.verify()?;
        attachments.verify()?;
        checkpoint(interrupted)?;
        fs::renameat(&folder.fd, ".meta.pending", &folder.fd, "meta.json").map_err(|_| UNSAFE)?;
        fs::fsync(&folder.fd).map_err(|_| UNSAFE)?;
        fs::fsync(&attachments.fd).map_err(|_| UNSAFE)?;
        checkpoint(interrupted)?;
        folder.verify()?;
        attachments.verify()?;
        Ok(())
    })();
    if result.is_err() {
        for name in ["meta.json", ".meta.pending", "content.txt"] {
            let _ = fs::unlinkat(&folder.fd, name, AtFlags::empty());
        }
        let _ = fs::unlinkat(&attachments.fd, &id, AtFlags::REMOVEDIR);
    }
    result?;
    Ok(record)
}
pub fn random_hex() -> Result<String> {
    random_id()
}
fn random_id() -> Result<String> {
    let mut bytes = [0u8; 10];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| UNSAFE)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn attachment_names(repo: &Path) -> Result<Vec<OsString>> {
    let path = repo.join(".oms/inputs/attachments");
    plain(&path)?;
    let folder = match std::fs::read_dir(&path) {
        Ok(folder) => folder,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(UNSAFE),
    };
    let mut result = Vec::new();
    for entry in folder {
        if result.len() >= 1000 {
            return Err("attachment directory exceeds the 1000-entry bound");
        }
        result.push(entry.map_err(|_| UNSAFE)?.file_name());
    }
    plain(&path)?;
    Ok(result)
}
pub fn metadata_exists(repo: &Path, id: &OsStr) -> Result<bool> {
    let attachments = inputs(repo, false)?.child(OsStr::new("attachments"), false)?;
    // Inspect the marker through anchored descriptors before imposing identity
    // checks on an incomplete folder that a concurrent writer can remove.
    let fd = match fs::openat(&attachments.fd, id, DIR_FLAGS, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
            return Ok(false);
        }
        Err(_) => return Err(UNSAFE),
    };
    match fs::statat(&fd, "meta.json", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(info) if FileType::from_raw_mode(info.st_mode) == FileType::RegularFile => {
            attachments.verify()?;
            Ok(true)
        }
        Ok(_) => Err(UNSAFE),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err(UNSAFE),
    }
}

const QUEUE: &str = "queue.json";
/// Stable file holding the exclusive queue lock; `queue.json` itself is replaced by rename.
const LOCK: &str = ".queue.lock";
pub const LOCK_SECONDS: u64 = 15;
pub struct Lock(#[allow(dead_code)] OwnedFd);
/// The caller has already refused a Git-tracked `.oms/inputs`.
pub fn lock(repo: &Path, interrupted: &AtomicBool) -> Result<Lock> {
    let inputs = ensure_ignored(repo, interrupted)?;
    let fd = fs::openat(
        &inputs.fd,
        LOCK,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| UNSAFE)?;
    let info = fs::fstat(&fd).map_err(|_| UNSAFE)?;
    if FileType::from_raw_mode(info.st_mode) != FileType::RegularFile
        || info.st_uid != rustix::process::getuid().as_raw()
    {
        return Err(UNSAFE);
    }
    let deadline = Instant::now() + Duration::from_secs(LOCK_SECONDS);
    loop {
        checkpoint(interrupted)?;
        match fs::flock(&fd, fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(Lock(fd)),
            Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {}
            Err(_) => return Err(UNSAFE),
        }
        if Instant::now() >= deadline {
            return Err(crate::LOCK_TIMEOUT);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
/// Fsync a private temporary, then rename it over `queue.json`; readers see the old or new file.
pub fn write_queue(repo: &Path, bytes: &[u8], interrupted: &AtomicBool) -> Result<()> {
    let inputs = ensure_ignored(repo, interrupted)?;
    let temporary = format!(".{QUEUE}.{}", random_id()?);
    let written = (|| {
        inputs.write_new(&temporary, bytes, interrupted)?;
        inputs.verify()?;
        checkpoint(interrupted)?;
        fs::renameat(&inputs.fd, &temporary, &inputs.fd, QUEUE).map_err(|_| UNSAFE)?;
        fs::fsync(&inputs.fd).map_err(|_| UNSAFE)
    })();
    if written.is_err() {
        let _ = fs::unlinkat(&inputs.fd, &temporary, AtFlags::empty());
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::Ordering;

    #[test]
    fn cleanup_after_content_write_on_interruption_and_metadata_error() {
        let path = std::env::temp_dir().join(format!(
            "oms-input-write-{}-{}",
            std::process::id(),
            random_id().unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let path = path.canonicalize().unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let interrupted = AtomicBool::new(false);
        let result = create_with_hook(
            &path,
            &interrupted,
            |_| json::J::Null,
            b"paragraph",
            || interrupted.store(true, Ordering::Relaxed),
        );
        assert!(result.is_err());
        assert!(attachment_names(&path).unwrap().is_empty());
        interrupted.store(false, Ordering::Relaxed);
        let mut deep = json::J::Null;
        for _ in 0..66 {
            deep = json::J::Array(vec![deep]);
        }
        let result = create(&path, &interrupted, |_| deep, b"paragraph");
        assert!(result.is_err());
        assert!(attachment_names(&path).unwrap().is_empty());
    }
}
