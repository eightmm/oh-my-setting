//! Fixed read-only Git metadata queries; no shell, interpreter, or hook runs.
use crate::scheduler::{self, Execution};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Fresh repository identity. A parent can supply the same metadata directly
/// through `run_with_repository` when it already owns this command snapshot.
pub struct Repository {
    pub root: PathBuf,
    pub commit: OsString,
}

/// getcwd is physical. PWD is only a spelling hint after proving that it names
/// the supplied directory; an explicit logical cwd takes precedence over it.
pub(crate) fn caller_spelling(cwd: &Path) -> Result<PathBuf, String> {
    let absolute = if cwd.is_absolute() {
        cwd.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| "cannot resolve working directory")?
            .join(cwd)
    };
    let physical =
        std::fs::canonicalize(&absolute).map_err(|_| "cannot resolve working directory")?;
    if absolute.as_os_str() != physical.as_os_str() {
        return Ok(absolute);
    }
    if let Some(value) = std::env::var_os("PWD") {
        let path = PathBuf::from(value);
        if path.is_absolute()
            && let (Ok(logical), Ok(real)) =
                (std::fs::metadata(&path), std::fs::metadata(&physical))
            && logical.is_dir()
            && logical.dev() == real.dev()
            && logical.ino() == real.ino()
            && std::fs::canonicalize(&path).is_ok_and(|p| p == physical)
        {
            return Ok(path);
        }
    }
    Ok(absolute)
}

fn query(
    caller: &Path,
    target: &Path,
    option: &str,
    interrupted: &AtomicBool,
) -> Result<(i32, Vec<u8>), String> {
    let args = [
        OsString::from("-C"),
        target.as_os_str().to_owned(),
        OsString::from("rev-parse"),
        OsString::from(option),
    ];
    let mut stdout = Vec::new();
    match scheduler::run(
        std::ffi::OsStr::new("git"),
        &args,
        caller,
        scheduler::Input::Closed,
        interrupted,
        &mut stdout,
        &mut std::io::sink(),
    ) {
        Execution::Answered {
            code,
            output_complete: true,
            ..
        } => Ok((code, stdout)),
        Execution::Answered {
            output_complete: false,
            ..
        } => Err("could not completely observe Git repository metadata".into()),
        Execution::Interrupted => Err("interrupted".into()),
        Execution::Unknown => Err("could not observe Git repository metadata".into()),
    }
}
fn substitution(mut bytes: Vec<u8>) -> OsString {
    // Bash command substitution removes every trailing LF, retaining all
    // other pathname/commit bytes, including interior newlines and CR.
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    OsString::from_vec(bytes)
}

/// Canonical `oms_repo_root` and HEAD observation, including Git's own ref
/// backend, configuration, environment and linked-worktree handling.
pub fn discover(cwd: &Path) -> Result<Repository, String> {
    discover_with_interrupt(cwd, &AtomicBool::new(false))
}

/// Same read-only snapshot with caller-owned cancellation. Root and HEAD are
/// each queried once; neither command can run a Git hook or modify an index.
pub fn discover_with_interrupt(cwd: &Path, interrupted: &AtomicBool) -> Result<Repository, String> {
    let logical = caller_spelling(cwd)?;
    let cwd = std::fs::canonicalize(cwd).map_err(|_| "cannot resolve working directory")?;
    // `oms_repo_root` ignores git's status but retains any stdout before
    // falling back to pwd when that stdout is empty.
    let (_, root) = query(&cwd, &cwd, "--show-toplevel", interrupted)?;
    let root = substitution(root);
    let root = if root.is_empty() {
        logical
    } else {
        cwd.join(root)
    };
    // Preserve stdout even on failure: in an unborn repository this is
    // exactly HEAD\nnogit rather than nogit alone.
    let (status, mut commit) = query(&cwd, &root, "HEAD", interrupted)?;
    if status != 0 {
        commit.extend_from_slice(b"nogit\n");
    }
    Ok(Repository {
        root,
        commit: substitution(commit),
    })
}
