//! Native state, ledger, workspace and local job commands.
#![forbid(unsafe_code)]

mod capsule;
mod checkpoints;
mod failures;
pub mod lease;
mod storage;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub use oms_command_plan::json::{self, Value};
pub use oms_command_plan::{Error, Result};
pub type Output = (i32, Vec<u8>, Vec<u8>);

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
    _source: &Path,
    interrupted: &AtomicBool,
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
            _ => Err(error(2, format!("native {tool} integration is incomplete"))),
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
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .map(|t| t.and_utc().timestamp())
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
