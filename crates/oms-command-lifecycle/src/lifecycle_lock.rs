use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const FIELDS: [&str; 4] = ["pid", "pid-start", "started", "owner"];
fn clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn snapshot(path: &Path) -> Result<Vec<String>> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o022 != 0
    {
        return r::fail("unsafe lifecycle lock directory");
    }
    FIELDS
        .iter()
        .map(|name| {
            let file = path.join(name);
            if r::exists(&file) {
                String::from_utf8(r::read(&file, 4096)?)
                    .map(|s| s.trim().to_owned())
                    .map_err(|_| r::Error(75, "invalid lifecycle lock metadata".into()))
            } else {
                Ok(String::new())
            }
        })
        .collect()
}
pub(crate) fn alive(pid: u32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return false;
    };
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}
fn stale(row: &[String], timeout: u64, paths: &Paths, flag: &AtomicBool) -> bool {
    match row[0].parse::<u32>().ok().filter(|n| *n > 0) {
        Some(pid) => {
            !alive(pid)
                || !row[1].is_empty()
                    && crate::update::process(pid, flag, &paths.cwd)
                        .is_some_and(|(s, _)| s != row[1])
        }
        None => row[2]
            .parse::<u64>()
            .is_ok_and(|s| clock().saturating_sub(s) >= timeout),
    }
}
fn remove_owned(path: &Path, expected: &[String]) -> Result<()> {
    if snapshot(path)? != expected {
        return r::fail("lifecycle lock ownership changed");
    }
    for name in FIELDS {
        match fs::remove_file(path.join(name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}
struct Claim {
    path: PathBuf,
    row: Vec<String>,
}
impl Drop for Claim {
    fn drop(&mut self) {
        let _ = remove_owned(&self.path, &self.row);
        let _ = fs::remove_dir(self.path.parent().unwrap());
    }
}
pub(crate) struct Guard {
    path: PathBuf,
    row: Option<Vec<String>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(row) = &self.row {
            let _ = remove_owned(&self.path, row);
        }
    }
}
fn initialize(path: &Path, row: &[String]) -> Result<()> {
    let original = fs::symlink_metadata(path)?;
    let result = (|| -> Result<()> {
        for (name, value) in FIELDS.iter().zip(row) {
            r::new_file(&path.join(name), format!("{value}\n").as_bytes(), 0o600)?;
        }
        r::sync_dir(path)
    })();
    if result.is_err()
        && fs::symlink_metadata(path)
            .is_ok_and(|m| m.dev() == original.dev() && m.ino() == original.ino())
    {
        for (name, expected) in FIELDS.iter().zip(row) {
            let file = path.join(name);
            if r::read(&file, 4096).is_ok_and(|raw| {
                expected
                    .as_bytes()
                    .starts_with(raw.strip_suffix(b"\n").unwrap_or(&raw))
            }) {
                let _ = fs::remove_file(file);
            }
        }
        let _ = fs::remove_dir(path);
    }
    result
}
pub(crate) fn acquire(paths: &Paths, flag: &AtomicBool) -> Result<Guard> {
    let path = std::env::var_os("OMS_INSTALL_LIFECYCLE_LOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("OMS_LOCK_DIR")
                .map(PathBuf::from)
                .unwrap_or(paths.home.join(".cache/oh-my-setting/locks"))
                .join("install-lifecycle.lock.d")
        });
    let path = if path.is_absolute() {
        r::leaf(&path)?
    } else {
        r::leaf(&paths.cwd.join(path))?
    };
    if path
        .file_name()
        .is_none_or(|s| s != "install-lifecycle.lock.d")
    {
        return r::fail("invalid lifecycle lock basename");
    }
    fs::create_dir_all(path.parent().unwrap())?;
    let pid = std::process::id();
    let generation = crate::update::process(pid, flag, &paths.cwd)
        .ok_or_else(|| {
            r::Error(
                75,
                "process generation unavailable for lifecycle locking".into(),
            )
        })?
        .0;
    if r::exists(&path) {
        let row = snapshot(&path)?;
        let holder = row[0].parse::<u32>().ok();
        let own = holder == Some(pid) && row[1] == generation;
        let explicit = std::env::var_os("OMS_INSTALL_LIFECYCLE_LOCK_HELD")
            .is_some_and(|v| v == "1")
            && std::env::var_os("OMS_INSTALL_LIFECYCLE_LOCK_PATH")
                .is_some_and(|v| Path::new(&v) == path)
            && std::env::var("OMS_INSTALL_LIFECYCLE_LOCK_OWNER").is_ok_and(|v| v == row[3]);
        let parent = holder
            == Some(
                rustix::process::getppid()
                    .map(|p| p.as_raw_pid() as u32)
                    .unwrap_or(0),
            )
            && holder.is_some_and(|p| {
                alive(p)
                    && crate::update::process(p, flag, &paths.cwd).is_some_and(|(g, _)| g == row[1])
            });
        let native = crate::update::borrowed(paths).unwrap_or(false)
            && holder.is_some_and(|p| {
                crate::update::ancestor(p, flag, paths)
                    && crate::update::process(p, flag, &paths.cwd).is_some_and(|(g, _)| g == row[1])
            });
        if explicit && (own || parent) || native {
            return Ok(Guard {
                path,
                row: if own { Some(row) } else { None },
            });
        }
    }
    let mut entropy = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut entropy)?;
    let random = entropy
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let owner = format!("{pid}.{}.{random}", clock());
    let row = vec![
        pid.to_string(),
        generation,
        clock().to_string(),
        owner.clone(),
    ];
    let timeout = oms_lock::legacy_seconds(
        std::env::var_os("OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT")
            .or_else(|| std::env::var_os("OMS_LOCK_TIMEOUT"))
            .as_deref(),
        300,
    );
    let start = Instant::now();
    let claims = path.with_file_name("install-lifecycle.lock.d.recovery-claims.d");
    if claims.is_symlink() {
        return r::fail("unsafe lifecycle recovery claims");
    }
    fs::create_dir_all(&claims)?;
    let claimpath = claims.join(format!("{}.{}", clock(), owner));
    fs::DirBuilder::new().mode(0o700).create(&claimpath)?;
    initialize(&claimpath, &row)?;
    let claim = Claim {
        path: claimpath.clone(),
        row: row.clone(),
    };
    loop {
        r::check(flag)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&claims)? {
            let entry = entry?;
            let path = entry.path();
            let state = snapshot(&path)?;
            if path != claim.path && stale(&state, timeout, paths, flag) {
                remove_owned(&path, &state)?;
                continue;
            }
            names.push(entry.file_name());
        }
        names.sort();
        if names.first().map(|s| s.as_os_str()) == claimpath.file_name() {
            break;
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            return Err(r::Error(75, "lifecycle recovery queue is busy".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    loop {
        r::check(flag)?;
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                let guard = Guard {
                    path: path.clone(),
                    row: Some(row.clone()),
                };
                initialize(&path, &row)?;
                drop(claim);
                return Ok(guard);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let previous = snapshot(&path)?;
        if stale(&previous, timeout, paths, flag) {
            let before = fs::symlink_metadata(&path)?;
            if snapshot(&path)? == previous {
                let retired =
                    path.with_file_name(format!("install-lifecycle.lock.d.stale.{owner}"));
                fs::rename(&path, &retired)?;
                let after = fs::symlink_metadata(&retired)?;
                if before.dev() != after.dev() || before.ino() != after.ino() {
                    if !r::exists(&path) {
                        let _ = fs::rename(&retired, &path);
                    }
                    return r::fail("lifecycle recovery identity changed; retained");
                }
                remove_owned(&retired, &previous)?;
                continue;
            }
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            return Err(r::Error(75, "install lifecycle is busy".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
