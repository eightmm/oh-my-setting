//! Native peer collaboration command adapters. Installation data is supplied
//! separately from the repository; neither process cwd nor environment changes.
#![forbid(unsafe_code)]

// Each literal call site owns one compiled expression for the process lifetime.
macro_rules! cached_regex {
    ($pattern:expr $(,)?) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| regex::Regex::new($pattern).expect("static regex"))
    }};
}

mod board;
pub mod cli;
mod context_pack;
mod delegate;
pub use delegate::buffer_interactive_stdout;
mod digest;
mod manifest;
mod ml_context;
mod native;
mod peer;
pub mod process;
pub mod providers;
mod receipts;
mod review;
mod review_red;
pub mod safety;
mod session;
pub mod storage;
mod thread;
mod view;

pub use oms_command_operations::value::{V, obj};
use std::{
    ffi::OsString,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
pub type Output = (i32, Vec<u8>, Vec<u8>);
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::new(2, "filesystem operation failed")
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::new(2, message)
    }
}

pub const TOOLS: &[&str] = &[
    "peer-ask",
    "peer-delegate",
    "peer-review",
    "consult",
    "advise",
    "agent-call",
    "thread",
    "session",
    "session-handoff",
    "artifact-view",
    "output-digest",
    "external",
    "cite",
    "data-manifest",
    "experiment-board",
];

/// Classification is pure and includes malformed argv for owned tools.
pub fn owns(tool: &str, _: &[OsString]) -> bool {
    TOOLS.contains(&tool)
}

/// Development adapter. Installed callers should pass their validated source
/// root to `run_with_source`, independently of the selected data repository.
pub fn run(tool: &str, args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Output {
    run_with_source(
        tool,
        args,
        cwd,
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../..")),
        interrupted,
    )
}

pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> Output {
    run_with_executor(tool, args, cwd, source_root, interrupted, &process::Native)
}

/// Injectable execution boundary for transport integration. Executor results
/// must distinguish completed output from timeout/cancellation/truncation.
pub fn run_with_executor(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
    executor: &dyn process::Executor,
) -> Output {
    if !owns(tool, args) {
        return (75, Vec::new(), Vec::new());
    }
    let result = (|| {
        check(interrupted)?;
        let context = Context {
            cwd,
            source: source_root,
            interrupted,
            executor,
        };
        match tool {
            "output-digest" => digest::run(args, &context),
            "external" | "cite" => receipts::run(tool, args, &context),
            "thread" => thread::run(args, &context),
            "experiment-board" => board::run(args, &context),
            "data-manifest" => manifest::run(args, &context),
            "peer-review" => review::run(args, &context),
            "session" | "session-handoff" => session::run(tool, args, &context),
            "artifact-view" => view::run(args, &context),
            _ => peer::run(tool, args, &context),
        }
    })();
    result.unwrap_or_else(|e: Error| {
        (
            e.code,
            Vec::new(),
            format!("error: {}\n", e.message).into_bytes(),
        )
    })
}

pub(crate) struct Context<'a> {
    pub cwd: &'a Path,
    pub source: &'a Path,
    pub interrupted: &'a AtomicBool,
    pub executor: &'a dyn process::Executor,
}
pub(crate) fn check(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Acquire) {
        Err(Error::new(130, "operation interrupted"))
    } else {
        Ok(())
    }
}
pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::new(2, message))
}
pub(crate) fn out(text: impl Into<Vec<u8>>) -> Output {
    (0, text.into(), Vec::new())
}
pub(crate) fn json(value: &V) -> Result<Vec<u8>> {
    let mut bytes = value.json(true)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub(crate) fn now() -> String {
    oms_command_room::now()
}
pub(crate) fn seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn text<'a>(value: &'a V, key: &str) -> &'a str {
    value.get(key).text().unwrap_or("")
}
pub(crate) fn number(value: &V) -> Option<f64> {
    if let V::Num(n) = value {
        n.parse::<f64>().ok().filter(|v| v.is_finite())
    } else {
        None
    }
}
pub(crate) fn id(prefix: &str) -> Result<String> {
    use std::io::Read;
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
