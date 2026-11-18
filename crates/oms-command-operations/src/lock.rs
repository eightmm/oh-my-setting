//! The canonical agent-events owner.json protocol, deliberately separate from oms-lock.
use crate::storage::{self, Ledger};
use crate::value::{V, obj};
use crate::{Result, checkpoint, digest, now_seconds, random_hex};
use rustix::fs::{self as rfs, FlockOperation, Mode, OFlags};
use std::fs::{self, File};
use std::io::{Read, Write};
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
    process_start_before(
        pid,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
    )
    .ok()
    .flatten()
}
fn probe_checkpoint(interrupted: &AtomicBool, deadline: Instant) -> Result<()> {
    checkpoint(interrupted)?;
    if Instant::now() >= deadline {
        return Err("state lock process proof deadline expired".into());
    }
    Ok(())
}
fn process_start_before(
    pid: u32,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Option<String>> {
    probe_checkpoint(interrupted, deadline)?;
    if pid == 0 {
        return Ok(None);
    }
    if let Some(fields) = proc_stat_fields(pid)
        && fields.len() >= 20
        && fields[19].bytes().all(|b| b.is_ascii_digit())
    {
        return Ok(Some(format!("proc:{}", fields[19])));
    }
    for program in ["/bin/ps", "/usr/bin/ps"] {
        if Path::new(program).is_file() {
            return process_start_ps_before(
                Path::new(program),
                pid,
                interrupted,
                deadline.min(Instant::now() + Duration::from_secs(2)),
            );
        }
    }
    Ok(None)
}
pub(crate) fn process_start_ps_before(
    program: &Path,
    pid: u32,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<Option<String>> {
    use std::process::{Child, Command, Stdio};
    struct Probe(Child);
    impl Drop for Probe {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    probe_checkpoint(interrupted, deadline)?;
    let Ok(child) = Command::new(program)
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .current_dir("/")
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Ok(None);
    };
    let mut probe = Probe(child);
    let stdout = probe
        .0
        .stdout
        .as_mut()
        .ok_or("process proof stdout unavailable")?;
    let flags = rfs::fcntl_getfl(&*stdout).map_err(|e| e.to_string())?;
    rfs::fcntl_setfl(&*stdout, flags | OFlags::NONBLOCK).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    let mut eof = false;
    let status = loop {
        probe_checkpoint(interrupted, deadline)?;
        if !eof {
            let mut buffer = [0u8; 256];
            match probe.0.stdout.as_mut().unwrap().read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    if raw.len() + count > 4096 {
                        return Err("process proof output exceeds bound".into());
                    }
                    raw.extend_from_slice(&buffer[..count]);
                    continue;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        if let Some(status) = probe.0.try_wait().map_err(|e| e.to_string())?
            && eof
        {
            break status;
        }
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    };
    probe_checkpoint(interrupted, deadline)?;
    if !status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!("ps:{value}")))
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
    stale_before(
        observed,
        unknown_age,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap_or(false)
}
fn stale_before(
    observed: &Snapshot,
    unknown_age: u64,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<bool> {
    probe_checkpoint(interrupted, deadline)?;
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
            return Ok(true);
        }
        if let Ok(pid) = u32::try_from(pid) {
            if cfg!(target_os = "linux")
                && proc_stat_fields(pid)
                    .is_some_and(|f| f.first().is_some_and(|s| matches!(s.as_str(), "Z" | "X")))
            {
                return Ok(true);
            }
            if let Some(recorded) = owner.get("process_start").text().filter(|s| !s.is_empty())
                && let Some(actual) = process_start_before(pid, interrupted, deadline)?
                && actual != recorded
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    Ok(now_seconds() - observed.mtime > unknown_age as f64)
}

pub fn reclaim(path: &Path, expected: &Snapshot, unknown_age: u64) -> Result<bool> {
    reclaim_before(
        path,
        expected,
        unknown_age,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
    )
}
fn reclaim_before(
    path: &Path,
    expected: &Snapshot,
    unknown_age: u64,
    interrupted: &AtomicBool,
    deadline: Instant,
) -> Result<bool> {
    probe_checkpoint(interrupted, deadline)?;
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
    if current.generation != expected.generation
        || !stale_before(&current, unknown_age, interrupted, deadline)?
    {
        return Ok(false);
    }
    let stale_path = path.with_file_name(format!(
        "{}.stale.{}",
        path.file_name()
            .ok_or("invalid lock name")?
            .to_string_lossy(),
        random_hex(8)?
    ));
    probe_checkpoint(interrupted, deadline)?;
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
    generation: String,
    pid: u32,
}
impl FileLock {
    pub fn acquire(path: &Path, interrupted: &AtomicBool) -> Result<Self> {
        let wait = seconds("OMS_LOCK_TIMEOUT", 300);
        Self::acquire_before(
            path,
            interrupted,
            Instant::now() + Duration::from_secs(wait),
        )
    }
    /// Shares the caller's remaining acquisition budget without changing the
    /// canonical owner protocol or expiring a live owner at that deadline.
    pub fn acquire_before(
        path: &Path,
        interrupted: &AtomicBool,
        deadline: Instant,
    ) -> Result<Self> {
        checkpoint(interrupted)?;
        if Instant::now() >= deadline {
            return Err("state lock acquisition deadline expired".into());
        }
        let lock = runtime_lock_dir(path)?;
        let unknown = seconds("OMS_LOCK_STALE_SECONDS", 300);
        let nonce = format!("{}.{}", std::process::id(), random_hex(8)?);
        let process_start =
            process_start_before(std::process::id(), interrupted, deadline)?.unwrap_or_default();
        loop {
            checkpoint(interrupted)?;
            if Instant::now() >= deadline {
                return Err(format!(
                    "could not acquire state lock for {}",
                    path.display()
                ));
            }
            match fs::create_dir(&lock) {
                Ok(()) => {
                    let identity = storage::stat_identity(&lock)?;
                    let owner = obj([
                        ("pid", (std::process::id() as usize).into()),
                        ("process_start", process_start.clone().into()),
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
                    let generation = snapshot(&lock)
                        .ok_or("new state lock ownership is unproven")?
                        .generation;
                    let held = Self {
                        path: lock,
                        nonce,
                        identity,
                        generation,
                        pid: std::process::id(),
                    };
                    held.check()?;
                    checkpoint(interrupted)?;
                    if Instant::now() >= deadline {
                        return Err("state lock acquisition deadline expired".into());
                    }
                    return Ok(held);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(observed) = snapshot(&lock)
                        && stale_before(&observed, unknown, interrupted, deadline)?
                        && reclaim_before(&lock, &observed, unknown, interrupted, deadline)?
                    {
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "could not acquire state lock for {}",
                            path.display()
                        ));
                    }
                    std::thread::sleep(
                        Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    /// Check the acquiring process, exact directory and owner generation before
    /// publishing across domains. A replacement is never adopted or removed.
    pub fn check(&self) -> Result<()> {
        if self.pid != std::process::id()
            || storage::stat_identity(&self.path).ok() != Some(self.identity)
            || snapshot(&self.path).is_none_or(|current| {
                current.generation != self.generation
                    || current.owner.get("owner_nonce").text() != Some(self.nonce.as_str())
            })
        {
            return Err("state lock ownership changed".into());
        }
        Ok(())
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        if self.check().is_err() {
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
