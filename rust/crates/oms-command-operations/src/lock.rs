//! The canonical agent-events owner.json protocol, deliberately separate from oms-lock.
use crate::storage::{self, Ledger};
use crate::value::{V, obj};
use crate::{Result, checkpoint, digest, now_seconds, random_hex};
use rustix::fs::{self as rfs, FlockOperation, Mode, OFlags};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub fn runtime_lock_dir(path: &Path) -> Result<PathBuf> {
    let base = match std::env::var_os("OMS_LOCK_DIR").filter(|s| !s.is_empty()) {
        Some(raw) => storage::expand(Path::new(&raw)),
        None => storage::home()?.join(".cache/oh-my-setting/locks"),
    };
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let key = digest(storage::resolved(path)?.as_os_str().as_bytes());
    Ok(base.join(format!("agent-events-{}.lockdir", &key[..32])))
}

fn seconds(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map_or(default, |n| n.max(1) as u64)
}

pub fn process_start(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    if let Some(fields) = proc_stat_fields(pid)
        && fields.len() >= 20
        && fields[19].bytes().all(|b| b.is_ascii_digit())
    {
        return Some(format!("proc:{}", fields[19]));
    }
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        None
    } else {
        Some(format!("ps:{value}"))
    }
}
pub fn proc_stat_fields(pid: u32) -> Option<Vec<String>> {
    let raw = fs::read_to_string(Path::new("/proc").join(pid.to_string()).join("stat")).ok()?;
    Some(
        raw.get(raw.rfind(')')? + 2..)?
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    )
}
pub fn pid_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    if pid > i32::MAX as i64 {
        return true;
    }
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return false;
    };
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

#[derive(Clone)]
pub struct Snapshot {
    pub generation: String,
    pub owner: V,
    pub mtime: f64,
}
fn identity(m: &fs::Metadata) -> (u64, u64, i128, i128) {
    (
        m.dev(),
        m.ino(),
        m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
        m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
    )
}
pub fn snapshot(path: &Path) -> Option<Snapshot> {
    for _ in 0..3 {
        let before = fs::symlink_metadata(path).ok()?;
        if !before.is_dir() {
            return None;
        }
        let raw = match storage::read_bytes(&path.join("owner.json")) {
            Ok(raw) => raw,
            Err(_) => return None,
        };
        let after = fs::symlink_metadata(path).ok()?;
        if identity(&before) != identity(&after) {
            continue;
        }
        let (dev, ino, mtime, ctime) = identity(&after);
        let mut generation = format!("({dev}, {ino}, {mtime}, {ctime})").into_bytes();
        generation.push(0);
        generation.extend(&raw);
        let owner = V::parse(&raw)
            .ok()
            .filter(|v| matches!(v, V::Obj(_)))
            .unwrap_or(V::Null);
        return Some(Snapshot {
            generation: digest(&generation),
            owner,
            mtime: after.mtime() as f64 + after.mtime_nsec() as f64 / 1e9,
        });
    }
    None
}
pub fn stale(observed: &Snapshot, unknown_age: u64) -> bool {
    let owner = &observed.owner;
    let raw_pid = crate::value::integer_value(&owner.default("pid", 0usize))
        .unwrap_or_else(|_| 0usize.into());
    // Out-of-range positive PIDs are deliberately unprovable, never dead.
    // The canonical liveness helper preserves these generations indefinitely.
    let pid = raw_pid
        .py()
        .parse::<i64>()
        .unwrap_or_else(|_| if raw_pid.integer(true) { i64::MAX } else { 0 });
    if pid > 0
        && owner
            .get("owner_nonce")
            .text()
            .is_some_and(|s| !s.is_empty())
    {
        if !pid_alive(pid) {
            return true;
        }
        if let Ok(pid) = u32::try_from(pid) {
            if cfg!(target_os = "linux")
                && proc_stat_fields(pid)
                    .is_some_and(|f| f.first().is_some_and(|s| matches!(s.as_str(), "Z" | "X")))
            {
                return true;
            }
            if let Some(recorded) = owner.get("process_start").text().filter(|s| !s.is_empty())
                && let Some(actual) = process_start(pid)
                && actual != recorded
            {
                return true;
            }
        }
        return false;
    }
    now_seconds() - observed.mtime > unknown_age as f64
}

pub fn reclaim(path: &Path, expected: &Snapshot, unknown_age: u64) -> Result<bool> {
    let gate = path.with_file_name(format!(
        "{}.recovery",
        path.file_name()
            .ok_or("invalid lock name")?
            .to_string_lossy()
    ));
    let descriptor: File = match rfs::open(
        &gate,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(fd) => fd.into(),
        Err(_) => return Ok(false),
    };
    let metadata = descriptor.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Ok(false);
    }
    let _ = descriptor.set_permissions(fs::Permissions::from_mode(0o600));
    if rfs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive).is_err() {
        return Ok(false);
    }
    let Some(current) = snapshot(path) else {
        return Ok(false);
    };
    if current.generation != expected.generation || !stale(&current, unknown_age) {
        return Ok(false);
    }
    let stale_path = path.with_file_name(format!(
        "{}.stale.{}",
        path.file_name()
            .ok_or("invalid lock name")?
            .to_string_lossy(),
        random_hex(8)?
    ));
    if fs::rename(path, &stale_path).is_err() {
        return Ok(false);
    }
    let _ = fs::remove_dir_all(stale_path);
    Ok(true)
}

pub struct FileLock {
    path: PathBuf,
    nonce: String,
    identity: (u64, u64),
}
impl FileLock {
    pub fn acquire(path: &Path, interrupted: &AtomicBool) -> Result<Self> {
        let lock = runtime_lock_dir(path)?;
        let wait = seconds("OMS_LOCK_TIMEOUT", 300);
        let unknown = seconds("OMS_LOCK_STALE_SECONDS", 300);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let nonce = format!("{}.{}", std::process::id(), random_hex(8)?);
        loop {
            checkpoint(interrupted)?;
            match fs::create_dir(&lock) {
                Ok(()) => {
                    let identity = storage::stat_identity(&lock)?;
                    let owner = obj([
                        ("pid", (std::process::id() as usize).into()),
                        (
                            "process_start",
                            process_start(std::process::id()).unwrap_or_default().into(),
                        ),
                        ("owner_nonce", nonce.clone().into()),
                        ("started", V::Num(now_seconds().to_string())),
                    ]);
                    let result = (|| {
                        let path = lock.join("owner.json");
                        let mut file: File = rfs::open(
                            &path,
                            OFlags::WRONLY
                                | OFlags::CREATE
                                | OFlags::EXCL
                                | OFlags::NOFOLLOW
                                | OFlags::CLOEXEC,
                            Mode::RUSR | Mode::WUSR,
                        )
                        .map_err(|e| e.to_string())?
                        .into();
                        file.write_all(&owner.json(false)?)
                            .map_err(|e| e.to_string())
                    })();
                    if let Err(error) = result {
                        if storage::stat_identity(&lock).ok() == Some(identity) {
                            let _ = fs::remove_dir_all(&lock);
                        }
                        return Err(error);
                    }
                    return Ok(Self {
                        path: lock,
                        nonce,
                        identity,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(observed) = snapshot(&lock)
                        && stale(&observed, unknown)
                        && reclaim(&lock, &observed, unknown)?
                    {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "could not acquire state lock for {}",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        if storage::stat_identity(&self.path).ok() != Some(self.identity) {
            return;
        }
        if let Ok(mut file) = Ledger::open(&self.path.join("owner.json"), false, false)
            && let Ok(raw) = file.read()
            && let Ok(owner) = V::parse(&raw)
            && owner.get("owner_nonce").text() == Some(self.nonce.as_str())
        {
            let _ = fs::remove_file(self.path.join("owner.json"));
            let _ = fs::remove_dir(&self.path);
        }
    }
}
