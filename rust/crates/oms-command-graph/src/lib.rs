#![forbid(unsafe_code)]
//! Native structural and execution graph command adapters.
mod cli;
pub mod execution;
pub mod notebook;
pub mod project;
mod util;

use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub type CommandResult = (i32, Vec<u8>, Vec<u8>);
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: 2,
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
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<oms_command_plan::Error> for Error {
    fn from(e: oms_command_plan::Error) -> Self {
        Self {
            code: e.code,
            message: e.message,
        }
    }
}

pub fn owns(_: &[OsString]) -> bool {
    true
}

pub fn run(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> CommandResult {
    run_with_helper(args, cwd, interrupted, None)
}

/// `python_helper` is an absolute packaged libexec path. No helper is searched
/// for on PATH. The explicit argument takes precedence over install metadata.
pub fn run_with_helper(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    python_helper: Option<&Path>,
) -> CommandResult {
    let args = match cli::parse(args) {
        Ok(v) => v,
        Err(result) => return result,
    };
    if interrupted.load(Ordering::SeqCst) {
        return (130, vec![], b"graph: interrupted\n".to_vec());
    }
    let work = || -> Result<CommandResult> {
        if std::env::var_os("OMS_HARNESS_CHILD").as_deref() == Some(std::ffi::OsStr::new("1"))
            && (!args.text("html_fragment").is_empty()
                || (args.group == "project" && args.action == "context")
                || (args.group == "exec"
                    && matches!(args.action.as_str(), "run" | "resume" | "decide" | "commit")))
        {
            return Err(Error::new(
                "a harness child may only use read-only graph actions",
            ));
        }
        let cwd = std::fs::canonicalize(cwd)?;
        let raw = args.text("repo");
        let selected = if raw.is_empty() {
            cwd.clone()
        } else {
            util::at(&cwd, raw)
        };
        let repo = util::repo_root(&selected, interrupted)?;
        match args.group.as_str() {
            "project" => project::dispatch(&args, &repo, &cwd, interrupted, python_helper),
            "exec" => execution::dispatch(&args, &repo, &cwd, interrupted, python_helper),
            _ => Err(Error::new("unknown graph group")),
        }
    };
    match work() {
        Ok(result) => result,
        Err(error) => (
            error.code,
            vec![],
            format!("graph: {}\n", error.message).into_bytes(),
        ),
    }
}
