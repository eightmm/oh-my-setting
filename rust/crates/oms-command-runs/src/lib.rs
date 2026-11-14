//! Native run-ledger and run-reconcile. The parent supplies shared service writers.
#![forbid(unsafe_code)]
mod ledger;
pub mod metrics;
mod process;
mod reconcile;
mod store;
mod store_path;
pub mod usage;
mod value;

pub use oms_context::Json;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
    fn lock(e: oms_lock::Error) -> Self {
        Self {
            code: e.exit_code(),
            message: e.to_string(),
        }
    }
}
impl From<String> for Error {
    fn from(s: String) -> Self {
        Self::usage(s)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::io(e.to_string())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

/// Execution identity used by experiment-run/launch and capsule companion consumers.
pub struct ExecutionRecord {
    pub row: Json,
    pub ledger: PathBuf,
    pub code: i32,
}
#[derive(Clone, Copy)]
pub enum Service {
    Journal,
    Memory,
    RunLink,
    Digest,
}
pub struct Observation<'a> {
    pub repo: &'a Path,
    pub source_type: &'a str,
    pub record_path: &'a Path,
    pub row: &'a Json,
    pub event_type: Option<&'a str>,
}
pub struct MemoryNote<'a> {
    pub repo: &'a Path,
    pub agent: &'a OsStr,
    pub text: &'a str,
}
pub struct RunLink<'a> {
    pub repo: &'a Path,
    pub run_id: &'a str,
    pub record_path: &'a Path,
    pub code: i32,
}
/// Scheduler evidence is passed once; a renderer must not query the scheduler again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingFormat {
    /// `sacct -X -D -j ID --format=COLUMNS -n -P`; legacy three-field rows remain recognizable.
    SacctAllocationParsable2,
}
pub struct AccountingSnapshot<'a> {
    pub format: AccountingFormat,
    pub columns: &'static str,
    pub available: bool,
    pub exit_code: i32,
    pub stdout: &'a [u8],
    pub stderr: &'a [u8],
}
pub struct DigestRequest<'a> {
    pub repo: &'a Path,
    pub job: &'a Json,
    pub accounting: AccountingSnapshot<'a>,
    pub interrupted: &'a AtomicBool,
}
/// Parent adapters call native journal/memory/run services in process. Missing services fail explicitly.
pub trait Services {
    fn available(&self, _service: Service) -> bool {
        false
    }
    fn observe(&mut self, _event: Observation<'_>) -> Result<()> {
        Err(Error::usage("native journal service is required"))
    }
    fn memory(&mut self, _note: MemoryNote<'_>) -> Result<()> {
        Err(Error::usage("native memory service is required"))
    }
    fn link(&mut self, _link: RunLink<'_>) -> Result<()> {
        Err(Error::usage("native run-link service is required"))
    }
    fn digest(&mut self, _request: DigestRequest<'_>) -> Result<Vec<u8>> {
        Err(Error::usage(
            "native captured-accounting digest service is required",
        ))
    }
}
/// Owns no writers and is suitable for read-only command families.
pub struct NoServices;
impl Services for NoServices {}

/// Explicit child environment overrides avoid mutating process globals in embedded callers.
pub struct Context<'a> {
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub interrupted: &'a AtomicBool,
    /// Optional actual signal, set by the parent signal handler (HUP/INT/TERM).
    pub signal: Option<&'a AtomicI32>,
    pub lock_settings: oms_lock::Settings,
    pub query_timeout: Duration,
    pub command_timeout: Option<Duration>,
}
impl<'a> Context<'a> {
    pub fn new(cwd: &Path, interrupted: &'a AtomicBool) -> Result<Self> {
        Ok(Self {
            cwd: cwd
                .canonicalize()
                .map_err(|_| Error::usage("working directory is unreadable"))?,
            env: BTreeMap::new(),
            interrupted,
            signal: None,
            lock_settings: oms_lock::Settings::from_env().map_err(Error::lock)?,
            query_timeout: Duration::from_secs(30),
            command_timeout: None,
        })
    }
    pub fn variable(&self, key: &str) -> Option<OsString> {
        self.env
            .get(OsStr::new(key))
            .cloned()
            .or_else(|| std::env::var_os(key))
    }
    pub fn setting(&self, key: &str) -> String {
        self.variable(key)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    pub fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
            || self.signal.is_some_and(|s| s.load(Ordering::Relaxed) != 0)
    }
    pub fn signal_code(&self) -> i32 {
        self.signal
            .map(|s| s.load(Ordering::Relaxed))
            .filter(|s| *s > 0)
            .map_or(130, |s| 128 + s)
    }
    pub fn check(&self) -> Result<()> {
        if self.interrupted() {
            Err(Error {
                code: self.signal_code(),
                message: "interrupted".into(),
            })
        } else {
            Ok(())
        }
    }
}
pub fn owns(command: &str) -> bool {
    matches!(command, "run-ledger" | "run-reconcile")
}
/// Streaming dispatch; child stdout/stderr go directly to the supplied writers.
pub fn run(
    command: &str,
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let result = match command {
        "run-ledger" => ledger::run(args, ctx, services, out, err),
        "run-reconcile" => reconcile::run(args, ctx, services, out, err),
        _ => Err(Error::usage("unknown run command")),
    };
    match result {
        Ok(code) => code,
        Err(e) if e.code == 0 => {
            let _ = out.write_all(e.message.as_bytes());
            0
        }
        Err(e) => {
            let _ = writeln!(err, "error: {}", e.message);
            e.code
        }
    }
}
/// In-process execution adapter used by trusted-local experiment and capsule consumers.
pub fn execute(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExecutionRecord> {
    ledger::execute(args, ctx, services, out, err)
}

#[cfg(test)]
mod tests;
