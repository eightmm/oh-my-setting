#![forbid(unsafe_code)]
//! Mixed-transition file locks. See the crate README before selecting an authority.
//! No signal handlers, shell helpers, environment changes or public writer cutover.

#[cfg(not(unix))]
compile_error!("oms-lock supports Unix hosts only");

mod identity;
mod paths;
mod protocol;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use protocol::Guard;

/// Explicit selection must match every other writer of the same state domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Like Bash: executable flock on PATH, unless OMS_LOCK_FORCE_MKDIR is 1.
    LegacyAuto,
    Flock,
    Mkdir,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: Mode,
    pub timeout: Duration,
    pub poll_max: Duration,
    pub lock_dir: Option<PathBuf>,
    pub home: PathBuf,
    pub temp_dir: PathBuf,
    pub cwd: PathBuf,
    pub search_path: OsString,
    pub force_mkdir: bool,
}

impl Settings {
    /// Snapshot ambient settings; never changes process globals.
    pub fn from_env() -> Result<Self, Error> {
        let get = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Ok(Self {
            mode: Mode::LegacyAuto,
            timeout: Duration::from_secs(legacy_seconds(get("OMS_LOCK_TIMEOUT").as_deref(), 300)),
            poll_max: Duration::from_secs(legacy_seconds(
                get("OMS_POLL_MAX_SECONDS").as_deref(),
                10,
            )),
            lock_dir: get("OMS_LOCK_DIR").map(PathBuf::from),
            home: get("HOME")
                .map(PathBuf::from)
                .ok_or(Error::InvalidSettings)?,
            temp_dir: get("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp")),
            cwd: std::env::current_dir().map_err(|_| Error::Io)?,
            search_path: get("PATH").unwrap_or_default(),
            force_mkdir: get("OMS_LOCK_FORCE_MKDIR").as_deref() == Some(OsStr::new("1")),
        })
    }

    fn selected_mode(&self) -> Mode {
        match self.mode {
            Mode::LegacyAuto if !self.force_mkdir && paths::flock_available(self) => Mode::Flock,
            Mode::LegacyAuto => Mode::Mkdir,
            other => other,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if self.timeout.is_zero()
            || self.poll_max.is_zero()
            || !self.home.is_absolute()
            || !self.temp_dir.is_absolute()
            || !self.cwd.is_absolute()
        {
            return Err(Error::InvalidSettings);
        }
        Ok(())
    }
}

/// Shell unsigned-decimal seconds; empty, zero, malformed or overflow uses default.
pub fn legacy_seconds(raw: Option<&OsStr>, default: u64) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    raw.and_then(|s| {
        let b = s.as_bytes();
        if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(b)
            .ok()?
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 0)
    })
    .unwrap_or(default)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidTarget,
    InvalidSettings,
    /// Deadline or nonblocking contention. Map to shell status 75.
    Busy,
    Cancelled,
    Unsafe,
    Changed,
    Io,
    UnsupportedIdentity,
}
impl Error {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::InvalidTarget | Self::InvalidSettings => 2,
            _ => 75,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file lock: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Held,
    Free,
    Unknown,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Free => "free",
            Self::Unknown => "unknown",
        }
    }
}

/// Shared cancellation flag. Cancellation never releases someone else's guard.
#[derive(Debug, Default)]
pub struct Cancellation(AtomicBool);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Immutable derivation only; fresh descriptors and permission checks per operation.
#[derive(Debug, Clone)]
pub struct Lock {
    path: PathBuf,
    mode: Mode,
    settings: Settings,
}
impl Lock {
    /// Exact Bash spelling/cksum authority, including raw Unix bytes.
    /// This does not canonicalize the state leaf: Bash does not do so either.
    pub fn for_file(target: &Path, settings: Settings) -> Result<Self, Error> {
        settings.validate()?;
        let path = paths::derive(target, &settings)?;
        Self::at_path(path, settings)
    }

    /// Low-level authority for a caller that already derives the canonical path.
    /// Runtime Python uses a different runtime-SHA256 name and always Mkdir.
    pub fn at_path(path: PathBuf, settings: Settings) -> Result<Self, Error> {
        settings.validate()?;
        if !path.is_absolute() || path.file_name().is_none() || path.as_os_str().is_empty() {
            return Err(Error::InvalidTarget);
        }
        paths::validate_path(&path)?;
        Ok(Self {
            path,
            mode: settings.selected_mode(),
            settings,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    /// Nonmutating: does not create directories, acquire flock, or reclaim.
    pub fn probe(&self) -> Status {
        protocol::probe(self).unwrap_or(Status::Unknown)
    }
    pub fn try_acquire(&self) -> Result<Guard, Error> {
        protocol::acquire(self, None, true)
    }
    pub fn acquire(&self, cancellation: Option<&Cancellation>) -> Result<Guard, Error> {
        protocol::acquire(self, cancellation, false)
    }
    /// Callback status is preserved; panic unwinding drops the guard.
    pub fn with_lock<T>(
        &self,
        cancellation: Option<&Cancellation>,
        action: impl FnOnce(&Guard) -> T,
    ) -> Result<T, Error> {
        let guard = self.acquire(cancellation)?;
        Ok(action(&guard))
    }
}

fn sleep_interval(elapsed: Duration, remaining: Duration, max: Duration) -> Duration {
    let seconds = match elapsed.as_secs() {
        0..=4 => 1,
        5..=29 => 2,
        30..=119 => 5,
        _ => 10,
    };
    Duration::from_secs(seconds).min(remaining).min(max)
}

fn wait_until(end: Instant, cancellation: Option<&Cancellation>) -> Result<(), Error> {
    loop {
        if cancellation.is_some_and(Cancellation::is_cancelled) {
            return Err(Error::Cancelled);
        }
        let Some(left) = end.checked_duration_since(Instant::now()) else {
            return Ok(());
        };
        // Wake for cancellation only; no filesystem or process polling here.
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_backoff_boundaries() {
        let remaining = Duration::from_secs(300);
        let maximum = Duration::from_secs(10);
        for (elapsed, expected) in [
            (0, 1),
            (4, 1),
            (5, 2),
            (29, 2),
            (30, 5),
            (119, 5),
            (120, 10),
        ] {
            assert_eq!(
                sleep_interval(Duration::from_secs(elapsed), remaining, maximum),
                Duration::from_secs(expected)
            );
        }
        assert_eq!(
            sleep_interval(remaining, Duration::ZERO, maximum),
            Duration::ZERO
        );
        assert_eq!(
            sleep_interval(remaining, Duration::from_secs(2), maximum),
            Duration::from_secs(2)
        );
        assert_eq!(
            sleep_interval(remaining, remaining, Duration::from_secs(1)),
            Duration::from_secs(1)
        );
        assert_eq!(paths::cksum(b"abc"), "1219131554-3");
        assert_eq!(paths::cksum(b""), "4294967295-0");
    }
}
