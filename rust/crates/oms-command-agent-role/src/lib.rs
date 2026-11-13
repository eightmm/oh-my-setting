//! Native `oms agent-role` handler. The installed entry must call
//! [`run_with_source`] with its separately validated installation root.
//! [`run`] uses the compile-time checkout only as a development fixture; it
//! never discovers authority through a user-controlled environment variable.
//! List collation uses one host `sort -u` with inherited locale. Its pipe input
//! and stdout are limited to 1 MiB, stderr to 64 KiB, and execution to 30 seconds.
//! A utility deadline returns 124; interruption returns 130 after kill/reap.

use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};

#[cfg(unix)]
mod native;
#[cfg(unix)]
mod policy;
#[cfg(unix)]
mod render;
#[cfg(unix)]
mod sort;
#[cfg(unix)]
mod write;

/// This command owns all invocations, including empty and malformed arguments.
/// Classification performs no reads, writes, or actions.
pub fn owns(_: &[OsString]) -> bool {
    true
}

pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let fixture = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."));
    let source = fixture
        .canonicalize()
        .unwrap_or_else(|_| fixture.to_path_buf());
    run_with_source(args, default_repo, &source, interrupted)
}

/// `source_root` is an internal installation-metadata dependency, supplied by
/// the parent entrypoint, independent of the user-selected data repository.
/// No provider is invoked and no process-global environment or cwd is changed.
pub fn run_with_source(
    args: &[OsString],
    data_cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    #[cfg(unix)]
    {
        native::run(args, data_cwd, source_root, interrupted)
    }
    #[cfg(not(unix))]
    {
        let _ = (args, data_cwd, source_root, interrupted);
        (
            2,
            Vec::new(),
            b"error: native Windows is not supported\n".to_vec(),
        )
    }
}
