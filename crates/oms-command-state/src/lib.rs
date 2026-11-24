//! Native state, ledger, workspace and local job commands.
#![forbid(unsafe_code)]

mod archive;
mod capsule;
mod checkpoints;
mod claims;
pub mod deps;
mod failures;
mod fleet;
mod gc;
mod gpu;
pub mod helpers;
pub mod inventory;
pub mod job_metrics;
pub mod lease;
mod queue;
mod recipe;
mod spine;
mod state;
mod storage;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub use oms_command_plan::json::{self, Value};
pub use oms_command_plan::{Error, Result};
pub type Output = (i32, Vec<u8>, Vec<u8>);
pub use failures::{observe_task_verification, task_failure_candidates};

pub const COMMANDS: &[&str] = &[
    "state",
    "state-verify",
    "gc",
    "lease",
    "inbox",
    "dashboard",
    "deps",
    "fail-ledger",
    "run",
    "run-capsule",
    "run-checkpoints",
    "run-claims",
    "tsp-queue",
    "recipe",
];

pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    COMMANDS.contains(&tool)
}

pub fn run(tool: &str, args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> Output {
    run_with_source(tool, args, cwd, cwd, interrupted)
}

pub fn run_with_source(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    interrupted: &AtomicBool,
) -> Output {
    let mut services = NativeServices {
        source,
        interrupted,
    };
    run_with_services(tool, args, cwd, interrupted, &mut services)
}

/// Host-provided callbacks for retention hooks whose state formats live in sibling crates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcHookResult {
    Reclaimed(Vec<(String, String)>),
    Deferred(String),
}

pub trait HostServices {
    /// Account-proof resolver for group-writable plan files; defaults to the plan services'.
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        self.plan_services().group_membership_resolver()
    }
    /// Services for opening the plan store; the default fails closed on group-writable plans.
    fn plan_services(&self) -> &(dyn oms_command_plan::Services + Sync) {
        &oms_command_plan::UnavailableServices
    }
    /// Native task status; `None` falls back to the crate's own `oms_command_task::run`.
    fn task_status(&self, _repo: &Path, _interrupted: &AtomicBool) -> Option<Output> {
        None
    }
    /// Optional display-only records from enrolled scratch checkouts.
    fn scratch_activity(&self, _repo: &Path, _interrupted: &AtomicBool) -> Option<Value> {
        None
    }
    /// Fresh canonical runtime evidence for this exact repository and snapshot.
    /// `None` keeps the library-only fallback; an error is a degraded source.
    fn runtime_envelope(&self, _repo: &Path, _interrupted: &AtomicBool) -> Option<Result<Value>> {
        None
    }
    fn gc_landing_captures(&self, _repo: &Path, _days: u64, _apply: bool) -> Result<GcHookResult> {
        Ok(GcHookResult::Deferred(
            "landing capture retention deferred: host service unavailable".into(),
        ))
    }
    fn gc_dispatch_logs(&self, _repo: &Path, _days: u64, _apply: bool) -> Result<GcHookResult> {
        Ok(GcHookResult::Deferred(
            "dispatch log retention deferred: host service unavailable".into(),
        ))
    }
}

pub struct NoHostServices;
impl HostServices for NoHostServices {}

/// The parent can inject the same native services used by run-ledger.
pub fn run_with_services(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &mut dyn oms_command_runs::Services,
) -> Output {
    run_with_host(tool, args, cwd, interrupted, services, &NoHostServices)
}

/// Entrypoint injecting both `oms-command-runs` services and host retention hooks.
pub fn run_with_host(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &mut dyn oms_command_runs::Services,
    host: &dyn HostServices,
) -> Output {
    let result = (|| {
        if !owns(tool, args) {
            return Err(error(2, "unknown state command"));
        }
        checkpoint(interrupted)?;
        let context = Context::new(cwd, interrupted)?;
        match tool {
            "lease" => lease::run(args, &context),
            "run-checkpoints" => checkpoints::run(args, &context),
            "fail-ledger" => failures::run(args, &context),
            "deps" => deps::run(args, &context),
            "state" | "state-verify" | "inbox" | "dashboard" => {
                state::run(tool, args, &context, host)
            }
            "gc" => gc::run(args, &context, host),
            "run-capsule" => capsule::run(args, &context, services),
            "run-claims" => claims::run(args, &context),
            "tsp-queue" => queue::run(args, &context, services),
            "recipe" => recipe::run(args, &context, services),
            "run" => match args.first().and_then(|s| s.to_str()) {
                Some("capsule") => capsule::run(&args[1..], &context, services),
                Some("checkpoints") => checkpoints::run(&args[1..], &context),
                Some("claims" | "figure") => claims::run(args, &context),
                _ => spine::run(args, &context, services),
            },
            _ => Err(error(2, "unknown state command")),
        }
    })();
    match result {
        Ok(output) => output,
        Err(e) => (
            e.code,
            Vec::new(),
            format!("error: {}\n", e.message).into_bytes(),
        ),
    }
}

/// Supply the canonical parent-owned runtime envelope without launching another CLI.
pub fn run_with_runtime(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    runtime: &Value,
) -> Output {
    run_with_runtime_host(tool, args, cwd, interrupted, runtime, &NoHostServices)
}

/// Like `run_with_runtime`, with host hooks for plan services and task status.
pub fn run_with_runtime_host(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    runtime: &Value,
    host: &dyn HostServices,
) -> Output {
    let result = (|| {
        require(
            ["state", "state-verify", "inbox", "dashboard"].contains(&tool),
            "runtime projection applies to state views",
        )?;
        let mut ctx = Context::new(cwd, interrupted)?;
        ctx.runtime = Some(runtime);
        state::run(tool, args, &ctx, host)
    })();
    result.unwrap_or_else(|e| (e.code, vec![], format!("error: {e}\n").into_bytes()))
}

/// Command help is available without opening a repository or acquiring a lock.
pub fn usage(tool: &str) -> Option<&'static str> {
    match tool {
        "run" => Some(spine::HELP),
        "run-capsule" => Some(capsule::HELP),
        "run-claims" => Some(claims::HELP),
        "deps" => Some(deps::HELP),
        "state" | "state-verify" | "inbox" | "dashboard" => Some(state::HELP),
        "gc" => Some(gc::HELP),
        "recipe" => Some(recipe::HELP),
        "tsp-queue" => Some(queue::HELP),
        "lease" => Some(lease::HELP),
        "run-checkpoints" => Some(checkpoints::HELP),
        "fail-ledger" => Some(failures::HELP),
        _ => None,
    }
}

fn parent_only() -> Result<()> {
    require(
        !["OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty() && v != "0")),
        "a harness child cannot mutate parent-owned host or global state; return the request to the parent agent",
    )
}

struct NativeServices<'a> {
    source: &'a Path,
    interrupted: &'a AtomicBool,
}
impl oms_command_runs::Services for NativeServices<'_> {
    fn available(&self, service: oms_command_runs::Service) -> bool {
        matches!(
            service,
            oms_command_runs::Service::Journal
                | oms_command_runs::Service::Memory
                | oms_command_runs::Service::RunLink
        )
    }
    fn observe(
        &mut self,
        event: oms_command_runs::Observation<'_>,
    ) -> oms_command_runs::Result<()> {
        let mut args = vec![
            "observe".into(),
            "--repo".into(),
            event.repo.as_os_str().to_owned(),
            "--source-type".into(),
            event.source_type.into(),
            "--source-file".into(),
            event.record_path.as_os_str().to_owned(),
        ];
        if let Some(kind) = event.event_type {
            args.extend(["--event-type".into(), kind.into()]);
        }
        service_result(oms_command_journal::run_with_source(
            &args,
            event.repo,
            self.source,
            self.interrupted,
        ))
    }
    fn memory(&mut self, note: oms_command_runs::MemoryNote<'_>) -> oms_command_runs::Result<()> {
        service_result(oms_command_memory::run_with_source(
            &[
                "append".into(),
                "--agent".into(),
                note.agent.to_owned(),
                note.text.into(),
            ],
            note.repo,
            self.source,
            self.interrupted,
        ))
    }
    fn link(&mut self, link: oms_command_runs::RunLink<'_>) -> oms_command_runs::Result<()> {
        let ctx = Context::new(link.repo, self.interrupted)
            .map_err(|e| oms_command_runs::Error::usage(e.to_string()))?;
        spine::link(
            &ctx,
            link.run_id,
            "run-ledger",
            "record",
            &link.record_path.to_string_lossy(),
            &format!("exit {}", link.code),
            &helpers::agent(),
        )
        .map_err(|e| oms_command_runs::Error::usage(e.to_string()))
    }
}
fn service_result(output: Output) -> oms_command_runs::Result<()> {
    if output.0 == 0 {
        Ok(())
    } else {
        Err(oms_command_runs::Error {
            code: output.0,
            message: String::from_utf8_lossy(&output.2).into_owned(),
        })
    }
}

fn error(code: i32, message: impl ToString) -> Error {
    Error {
        code,
        message: message.to_string(),
    }
}

fn require(condition: bool, message: impl ToString) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(error(2, message))
    }
}

fn checkpoint(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err(error(130, "interrupted"))
    } else {
        Ok(())
    }
}

struct Context<'a> {
    runtime: Option<&'a Value>,
    cwd: PathBuf,
    repo: PathBuf,
    state_repo: PathBuf,
    interrupted: &'a AtomicBool,
}

impl<'a> Context<'a> {
    fn new(cwd: &Path, interrupted: &'a AtomicBool) -> Result<Self> {
        let cwd = cwd.canonicalize().map_err(|e| error(2, e))?;
        let repo =
            oms_command_operations::storage::repo_root(&cwd, &cwd).map_err(|e| error(2, e))?;
        let state_repo = match std::env::var_os("OMS_STATE_REPO").filter(|v| !v.is_empty()) {
            Some(path) => oms_command_operations::storage::repo_root(Path::new(&path), &cwd)
                .map_err(|e| error(2, e))?,
            None => repo.clone(),
        };
        Ok(Self {
            runtime: None,
            cwd,
            repo,
            state_repo,
            interrupted,
        })
    }
}

fn success(text: impl AsRef<str>) -> Output {
    (0, text.as_ref().as_bytes().to_vec(), Vec::new())
}

fn json_output(code: i32, value: &Value) -> Result<Output> {
    let mut bytes = json::pretty(value)?;
    bytes.push(b'\n');
    Ok((code, bytes, Vec::new()))
}

fn timestamp(epoch: i64) -> Result<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .ok_or_else(|| error(2, "timestamp out of range"))
}

fn epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn parse_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|t| t.timestamp())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc().timestamp())
        })
}

fn strings(args: &[OsString]) -> Result<Vec<String>> {
    args.iter()
        .map(|v| {
            v.to_str()
                .map(str::to_owned)
                .ok_or_else(|| error(2, "argument is not UTF-8"))
        })
        .collect()
}

fn random_id(prefix: &str) -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| error(1, e))?;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

#[derive(Default)]
struct Options {
    pos: Vec<String>,
    values: std::collections::BTreeMap<String, Vec<String>>,
    flags: std::collections::BTreeSet<String>,
}
impl Options {
    fn parse(args: &[OsString], values: &[&str], flags: &[&str]) -> Result<Self> {
        let args = strings(args)?;
        let mut result = Self::default();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--" {
                result.pos.extend_from_slice(&args[i + 1..]);
                break;
            }
            if matches!(arg.as_str(), "-h" | "--help") {
                result.flags.insert("help".into());
            } else if let Some(flag) = arg.strip_prefix("--") {
                let (name, inline) = flag
                    .split_once('=')
                    .map_or((flag, None), |(k, v)| (k, Some(v)));
                if values.contains(&name) {
                    let value = match inline {
                        Some(v) => v.to_owned(),
                        None => {
                            i += 1;
                            args.get(i)
                                .ok_or_else(|| error(2, format!("--{name} requires a value")))?
                                .clone()
                        }
                    };
                    result.values.entry(name.into()).or_default().push(value);
                } else if flags.contains(&name) && inline.is_none() {
                    result.flags.insert(name.into());
                } else {
                    return Err(error(2, format!("unknown argument: {arg}")));
                }
            } else if arg.starts_with('-') {
                return Err(error(2, format!("unknown argument: {arg}")));
            } else {
                result.pos.push(arg.clone());
            }
            i += 1;
        }
        Ok(result)
    }
    fn get(&self, name: &str) -> &str {
        self.values
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
            .unwrap_or("")
    }
    fn all(&self, name: &str) -> &[String] {
        self.values.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
    fn number(&self, name: &str, default: usize) -> Result<usize> {
        if self.get(name).is_empty() {
            return Ok(default);
        }
        self.get(name)
            .parse()
            .map_err(|_| error(2, format!("--{name} must be a non-negative integer")))
    }
    fn repo(&self, ctx: &Context<'_>) -> Result<PathBuf> {
        if self.get("repo").is_empty() {
            return Ok(ctx.repo.clone());
        }
        oms_command_operations::storage::repo_root(Path::new(self.get("repo")), &ctx.cwd)
            .map_err(|e| error(2, e))
    }
}

/// Internal detached queue worker protocol; never a recipe or provider launcher.
pub fn run_queue_worker(request: &Path) -> i32 {
    match queue::worker(request) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.code
        }
    }
}
