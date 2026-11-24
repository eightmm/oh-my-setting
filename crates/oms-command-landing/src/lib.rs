//! Native landing, delivery and local recovery commands. The embedding CLI
//! supplies cross-domain producers through [`Services`]; missing authority
//! refuses the operation rather than fabricating a successful observation.
#![forbid(unsafe_code)]

pub mod admission;
pub mod attribution;
mod batch_preview;
pub mod candidate;
pub mod checkpoint;
pub mod ci;
mod cli;
pub mod delivery;
pub mod feedback;
mod git;
mod github;
pub mod guard;
pub mod helpers;
pub mod landing;
pub mod managed;
mod manifest;
pub mod notes;
mod reconcile;
pub mod red;
pub mod report;
pub mod review;
pub mod services;
mod storage;
mod suggestions;
pub mod worktree;

pub use oms_command_operations::value::{V, obj};
pub use services::{Services, Unavailable};
use std::{
    ffi::OsString,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub const TOOLS: &[&str] = &[
    "patch-admit",
    "patch-land",
    "land",
    "change-guard",
    "checkpoint",
    "candidate",
    "scratch-worktree",
    "cleanup",
    "sync-shared",
    "draft-pr",
    "ci-status",
    "report",
    "github",
];
#[derive(Debug, Clone)]
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
    pub fn unavailable(domain: &str) -> Self {
        Self::new(
            2,
            format!("native {domain} service unavailable; parent CLI integration is required"),
        )
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
impl From<String> for Error {
    fn from(e: String) -> Self {
        Self::new(2, e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(2, e.to_string())
    }
}
impl From<oms_command_plan::Error> for Error {
    fn from(e: oms_command_plan::Error) -> Self {
        Self::new(e.code, e.message)
    }
}
impl From<oms_command_task::Error> for Error {
    fn from(e: oms_command_task::Error) -> Self {
        Self::new(e.code, e.to_string())
    }
}

#[derive(Debug, Default)]
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Output {
    pub fn text(s: impl AsRef<str>) -> Self {
        Self {
            stdout: s.as_ref().as_bytes().to_vec(),
            ..Self::default()
        }
    }
    pub fn json(v: &V) -> Result<Self> {
        let mut b = v.json(true)?;
        b.push(b'\n');
        Ok(Self {
            stdout: b,
            ..Self::default()
        })
    }
}
pub fn owns(tool: &str, _: &[OsString]) -> bool {
    TOOLS.contains(&tool)
}
pub fn usage(tool: &str) -> Option<&'static str> {
    cli::usage(tool)
}
pub fn run(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_source(tool, args, cwd, cwd, interrupted)
}
pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(tool, args, cwd, source, interrupted, &Unavailable)
}
pub fn run_with_services(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> (i32, Vec<u8>, Vec<u8>) {
    let result = (|| {
        check(interrupted)?;
        if !owns(tool, args) {
            return Err(Error::new(2, format!("unknown command: {tool}")));
        }
        let options = cli::parse(tool, args)?;
        if options.has("help") {
            return Ok(Output::text(cli::usage(tool).unwrap()));
        }
        let _groups = storage::with_group_resolver(
            services
                .plan()
                .and_then(|plan| plan.group_membership_resolver()),
        );
        let context = Context {
            cwd,
            source,
            interrupted,
            services,
        };
        match tool {
            "patch-admit" => admission::execute(&context, &options),
            "patch-land" => landing::execute(&context, &options),
            "land" => delivery::execute(&context, &options),
            "change-guard" => guard::execute(&context, &options),
            "checkpoint" => checkpoint::execute(&context, &options),
            "candidate" => candidate::execute(&context, &options),
            "scratch-worktree" => worktree::execute(&context, &options),
            "cleanup" => worktree::cleanup(&context, &options),
            "sync-shared" => delivery::sync(&context, &options),
            "draft-pr" => review::execute(&context, &options),
            "ci-status" => ci::execute(&context, &options),
            "report" => report::execute(&context, &options),
            "github" => github::execute(&context, &options),
            _ => unreachable!(),
        }
    })();
    match result {
        Ok(o) => (o.code, o.stdout, o.stderr),
        Err(e) => (
            e.code,
            vec![],
            format!("error: {}\n", e.message).into_bytes(),
        ),
    }
}
pub(crate) struct Context<'a> {
    pub cwd: &'a Path,
    pub source: &'a Path,
    pub interrupted: &'a AtomicBool,
    pub services: &'a dyn Services,
}
impl Context<'_> {
    fn repo(&self, options: &cli::Options) -> Result<std::path::PathBuf> {
        let p = options
            .path("repo", self.cwd)
            .unwrap_or_else(|| self.cwd.to_owned());
        git::Git::root(&p, self.interrupted)
    }
    fn state(&self, repo: &Path) -> Result<std::path::PathBuf> {
        Ok(
            oms_reference::repository::Resolver::from_env()?.state_repository(
                repo,
                false,
                self.interrupted,
            )?,
        )
    }
}
pub(crate) fn check(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Relaxed) {
        Err(Error::new(130, "interrupted"))
    } else {
        Ok(())
    }
}
pub(crate) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::new(2, message))
    }
}
pub(crate) fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
pub(crate) fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn digest(raw: &[u8]) -> String {
    oms_reference::digest(raw)
}
pub(crate) fn string(v: &V, name: &str) -> String {
    v.get(name).text().unwrap_or_default().into()
}
pub(crate) fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub(crate) fn child() -> bool {
    ["OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}
pub(crate) fn parent() -> Result<()> {
    require(
        !child(),
        "a harness child cannot acquire parent landing or publication authority",
    )
}
pub(crate) fn safe(raw: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let env: Vec<_> = std::env::vars_os().collect();
    let env: Vec<_> = env
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    match oms_scrub::scan(raw, &env) {
        Ok(hits) if hits.is_empty() => Ok(()),
        _ => Err(Error::new(3, "sensitive-content scanner refused the draft")),
    }
}
