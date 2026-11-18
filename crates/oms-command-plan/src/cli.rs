//! `agent-plan` command-line entrypoint. Never touches process-global cwd,
//! environment or signal state: relative operands resolve against the caller's `cwd`.
use crate::repository::Git;
use crate::request::{BOOLEANS, NAMES};
use crate::{
    Action, Context, Plan, Request, Response, Services, USAGE, UnavailableServices, error, require,
};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn owns(tool: &str, _args: &[OsString]) -> bool {
    tool == "agent-plan"
}

pub fn run(args: &[OsString], cwd: &Path, interrupted: &AtomicBool) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, cwd, interrupted, &UnavailableServices)
}

pub fn run_with_services(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> (i32, Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with_io(args, cwd, interrupted, services, &mut out, &mut err);
    (code, out, err)
}

enum Scan {
    Help,
    NoAction,
    Parsed,
}

/// Walks the arguments in order like the shell parser: `-h`/`--help` wins only
/// when every earlier word is well formed, so it never masks a malformed token.
fn scan(args: &[OsString]) -> crate::Result<Scan> {
    let (mut at, mut action) = (0, false);
    let mut seen = Vec::new();
    while at < args.len() {
        let word = args[at]
            .to_str()
            .ok_or_else(|| error("unknown non-text argument"))?;
        if word == "-h" || word == "--help" {
            return Ok(Scan::Help);
        }
        if let Some(o) = word.strip_prefix("--") {
            if BOOLEANS.contains(&o) {
                at += 1;
            } else if NAMES.contains(&o) {
                if !args.get(at + 1).is_some() {
                    return Err(error(format!("{word} requires value")));
                }
                let operand = args[at + 1].to_str().unwrap_or("\u{1}");
                let digits = operand.bytes().all(|b| b.is_ascii_digit());
                let number = operand.parse::<u64>().ok();
                match o {
                    "priority" => {
                        require(operand.parse::<i32>().is_ok(), "--priority must be an i32")?
                    }
                    "ttl" => require(
                        digits && number.is_some(),
                        "--ttl must be an integer number of seconds",
                    )?,
                    "limit" => require(
                        number.is_some_and(|n| n > 0) && digits,
                        "--limit must be a positive integer",
                    )?,
                    "max-tasks" => require(
                        digits && number.is_some_and(|n| (1..=12).contains(&n)),
                        "--max-tasks must be 1..12",
                    )?,
                    "landed-commit" => {
                        require(!operand.is_empty(), "--landed-commit requires SHA")?
                    }
                    _ => {}
                }
                if ["depends", "allowed", "forbidden"].contains(&o) {
                    require(
                        !seen.contains(&o),
                        format!("--{o} given twice; pass one comma-separated list \"p1,p2\""),
                    )?;
                    seen.push(o);
                }
                at += 2;
            } else {
                return Err(error(format!("unknown argument: {word}")));
            }
        } else if Action::ALL.iter().any(|(name, _)| *name == word) {
            require(!action, "multiple commands")?;
            action = true;
            at += 1;
        } else {
            return Err(error(format!("unknown argument: {word}")));
        }
    }
    Ok(if action { Scan::Parsed } else { Scan::NoAction })
}

pub(crate) fn canonicalize_dir(path: &Path) -> std::io::Result<PathBuf> {
    // Kernel stat enforces ENOTDIR on `regular_file/..` components that
    // Darwin's userland realpath(3) otherwise collapses lexically.
    let meta = std::fs::metadata(path)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other("not a directory"));
    }
    std::fs::canonicalize(path)
}

/// Absolute physical directory spelling (`pwd -P` semantics). Components are
/// resolved left to right: each existing one is canonicalized at once, so a
/// following `..` steps out of the symlink's physical target, never out of its
/// spelling. Unresolved components are preserved so traversal cannot bypass them.
pub(crate) fn physical_dir(cwd: &Path, path: &OsStr) -> PathBuf {
    let joined = cwd.join(path);
    if !joined.is_absolute() {
        return joined;
    }
    let mut base = PathBuf::from("/");
    let mut missing: Vec<OsString> = Vec::new();
    for c in joined.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => {
                base = PathBuf::from("/");
                missing.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if missing.is_empty() && base.is_dir() {
                    base = base.parent().unwrap_or(&base).to_path_buf();
                } else {
                    missing.push(OsString::from(".."));
                }
            }
            Component::Normal(seg) => {
                if missing.is_empty()
                    && let Ok(real) = canonicalize_dir(&base.join(seg))
                {
                    base = real;
                } else {
                    missing.push(seg.to_owned());
                }
            }
        }
    }
    missing.iter().fold(base, |p, t| p.join(t))
}

/// Physical spelling of a file or marker path: only the parent directory is
/// resolved. The leaf is never canonicalized, so a symlinked leaf stays a
/// symlink for the no-follow storage checks.
pub(crate) fn physical_leaf(cwd: &Path, path: &OsStr) -> PathBuf {
    let joined = cwd.join(path);
    match (joined.file_name(), joined.parent()) {
        (Some(name), Some(parent)) if !joined.ends_with("..") && !joined.ends_with(".") => {
            physical_dir(cwd, parent.as_os_str()).join(name)
        }
        _ => physical_dir(cwd, path),
    }
}

/// Git worktree root of `repo` (physical), or the directory itself outside
/// git. Uses the shared sanitized Git runner, so ambient `GIT_*` cannot
/// redirect it and interruption/timeouts fail the call.
pub(crate) fn worktree_root(repo: &Path, interrupted: &AtomicBool) -> crate::Result<PathBuf> {
    worktree_root_before(repo, interrupted, None)
}
pub(crate) fn worktree_root_before(
    repo: &Path,
    interrupted: &AtomicBool,
    deadline: Option<std::time::Instant>,
) -> crate::Result<PathBuf> {
    let physical =
        canonicalize_dir(repo).map_err(|_| error("cannot resolve the physical repository"))?;
    let git = Git {
        group_snapshot: None,
        deadline,
        repo: &physical,
        interrupted,
        group_resolver: None,
    };
    let out = git.output(
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        None,
        &[],
        4096,
    )?;
    let mut top = out.stdout;
    while matches!(top.last(), Some(b'\n' | b'\r')) {
        top.pop();
    }
    if out.code != 0 || top.is_empty() {
        return Ok(physical);
    }
    canonicalize_dir(Path::new(&OsString::from_vec(top)))
        .map_err(|_| error("cannot resolve the physical repository"))
}

pub fn run_with_io(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let response = match scan(args) {
        Err(e) => Response::failure(e),
        Ok(Scan::Help) => Response::text(USAGE.trim_end_matches('\n')),
        Ok(Scan::NoAction) => Response {
            code: 2,
            stderr: USAGE.as_bytes().to_vec(),
            ..Response::default()
        },
        Ok(Scan::Parsed) => {
            dispatch(args, cwd, interrupted, services).unwrap_or_else(Response::failure)
        }
    };
    let wrote = out
        .write_all(&response.stdout)
        .and_then(|()| err.write_all(&response.stderr))
        .and_then(|()| out.flush())
        .and_then(|()| err.flush());
    if wrote.is_err() && response.code == 0 {
        2
    } else {
        response.code
    }
}

fn dispatch(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    services: &dyn Services,
) -> crate::Result<Response> {
    require(!interrupted.load(Ordering::SeqCst), "interrupted")?;
    let req = Request::parse(args)?;
    let cwd = canonicalize_dir(cwd).map_err(|_| error("cannot resolve current directory"))?;
    let cwd = cwd.as_path();
    // The shell default is `${OMS_STATE_REPO:-$PWD}`; the context normalises it.
    let default_repo = std::env::var_os("OMS_STATE_REPO")
        .filter(|s| !s.is_empty())
        .map_or_else(|| cwd.to_path_buf(), PathBuf::from);
    let context = Context::resolve(&req, &default_repo, cwd, interrupted, &|repo| {
        services.state_root(repo)
    })?;
    require(!interrupted.load(Ordering::SeqCst), "interrupted")?;
    Plan::open(context, services, interrupted)?.execute(&req)
}
