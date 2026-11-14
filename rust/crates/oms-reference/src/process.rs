//! Positive native process-generation proof. Unavailability is not liveness.
use crate::{Result, V, obj};
use std::fmt;
#[cfg(target_os = "linux")]
use std::{fs, os::unix::fs::MetadataExt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub pid: u32,
    pub birth: String,
    pub uid: u32,
}
#[derive(Debug)]
pub enum OwnerError {
    Exited,
    Unproven(String),
}
impl fmt::Display for OwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited => write!(f, "reference owner exited"),
            Self::Unproven(message) => write!(f, "{message}"),
        }
    }
}
impl Owner {
    pub fn value(&self) -> V {
        obj([
            ("pid", (self.pid as usize).into()),
            ("birth", self.birth.clone().into()),
            ("uid", (self.uid as usize).into()),
        ])
    }
    pub fn parse(value: &V) -> Result<Self> {
        let pid = value.get("pid");
        let uid = value.get("uid");
        if !matches!(value,V::Obj(v) if v.len() == 3) || !pid.integer(true) || !uid.integer(false) {
            return Err("invalid reference owner".into());
        }
        let pid = pid
            .py()
            .parse::<u32>()
            .map_err(|_| "invalid reference owner")?;
        let uid = uid
            .py()
            .parse::<u32>()
            .map_err(|_| "invalid reference owner")?;
        if pid == 0 || pid > i32::MAX as u32 || uid != rustix::process::getuid().as_raw() {
            return Err("invalid or foreign reference owner".into());
        }
        let birth = value
            .get("birth")
            .text()
            .filter(|v| !v.is_empty())
            .ok_or("reference owner generation unavailable")?
            .to_owned();
        Ok(Self { pid, birth, uid })
    }
}

/// Implementations must supply a kernel birth generation, never ps lstart,
/// room membership, a PID-only probe or an inherited environment assertion.
/// NativeIdentity uses procfs on Linux and a size-checked BSD record on macOS.
pub trait IdentitySource: Send + Sync {
    fn owner_identity(&self, pid: u32) -> std::result::Result<Owner, OwnerError>;
    fn foreground_parent(&self) -> std::result::Result<Owner, OwnerError> {
        let pid = rustix::process::getppid()
            .ok_or_else(|| OwnerError::Unproven("native foreground parent unavailable".into()))?;
        self.owner_identity(pid.as_raw_nonzero().get() as u32)
    }
}
pub struct NativeIdentity;
impl IdentitySource for NativeIdentity {
    fn owner_identity(&self, pid: u32) -> std::result::Result<Owner, OwnerError> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(OwnerError::Unproven("invalid reference owner".into()));
        }
        #[cfg(target_os = "linux")]
        {
            let path = std::path::Path::new("/proc").join(pid.to_string());
            let before = fs::metadata(&path).map_err(observation_error)?;
            let raw = fs::read_to_string(path.join("stat")).map_err(observation_error)?;
            let fields = raw
                .get(
                    raw.rfind(')').ok_or_else(|| {
                        OwnerError::Unproven("reference owner unavailable".into())
                    })? + 2..,
                )
                .ok_or_else(|| OwnerError::Unproven("reference owner unavailable".into()))?
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.first().is_some_and(|v| matches!(*v, "Z" | "X")) {
                return Err(OwnerError::Exited);
            }
            let start = fields
                .get(19)
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| {
                    OwnerError::Unproven("reference owner generation unavailable".into())
                })?;
            if before.uid() != rustix::process::getuid().as_raw() {
                return Err(OwnerError::Unproven(
                    "reference owner unproven or foreign".into(),
                ));
            }
            let boot =
                fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(observation_error)?;
            let boot = boot.trim();
            if boot.is_empty() {
                return Err(OwnerError::Unproven(
                    "reference owner generation unavailable".into(),
                ));
            }
            let after = fs::metadata(&path).map_err(observation_error)?;
            if !crate::storage::same(&before, &after) || before.uid() != after.uid() {
                return Err(OwnerError::Unproven(
                    "reference owner changed during observation".into(),
                ));
            }
            Ok(Owner {
                pid,
                birth: format!("{boot}:{start}"),
                uid: before.uid(),
            })
        }
        #[cfg(target_os = "macos")]
        {
            use proc_pidinfo::{Pid, ProcBSDInfo, proc_pidinfo};
            let info = match proc_pidinfo::<ProcBSDInfo>(Pid(pid)) {
                Ok(Some(info)) => info,
                Ok(None) => {
                    // This wrapper preserves no error object on zero. Capture
                    // thread errno before any formatting or secondary query.
                    let error = std::io::Error::last_os_error();
                    return if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) {
                        Err(OwnerError::Exited)
                    } else {
                        Err(OwnerError::Unproven("reference owner unavailable".into()))
                    };
                }
                Err(_) => return Err(OwnerError::Unproven("reference owner unavailable".into())),
            };
            if info.pbi_pid.0 != pid || info.pbi_uid != rustix::process::getuid().as_raw() {
                return Err(OwnerError::Unproven(
                    "reference owner unproven or foreign".into(),
                ));
            }
            if info.pbi_status == 5 {
                return Err(OwnerError::Exited);
            }
            if info.pbi_start_tvsec == 0 || info.pbi_start_tvusec >= 1_000_000 {
                return Err(OwnerError::Unproven(
                    "reference owner generation unavailable".into(),
                ));
            }
            Ok(Owner {
                pid,
                birth: format!("darwin:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
                uid: info.pbi_uid,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(OwnerError::Unproven(
                "native reference owner identity unavailable on this host".into(),
            ))
        }
    }
}
#[cfg(target_os = "linux")]
fn observation_error(error: std::io::Error) -> OwnerError {
    if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(3) {
        OwnerError::Exited
    } else {
        OwnerError::Unproven("reference owner unavailable".into())
    }
}

/// Forced termination uses a generation-pinned native handle. It never sends
/// a signal to an observed PID or process group after that proof is released.
pub fn terminate_monitor(source: &dyn IdentitySource, identity: &Owner) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
        let pid = Pid::from_raw(identity.pid as i32).ok_or("invalid reference monitor")?;
        let fd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::SRCH) => return Ok(true),
            Err(rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL | rustix::io::Errno::PERM) => {
                return Ok(false);
            }
            Err(error) => return Err(error.to_string()),
        };
        match source.owner_identity(identity.pid) {
            Ok(current) if current == *identity => {}
            Err(OwnerError::Exited) => return Ok(true),
            Ok(_) => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
        match pidfd_send_signal(fd, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(true),
            Err(error) => Err(error.to_string()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, identity);
        Ok(false)
    }
}
