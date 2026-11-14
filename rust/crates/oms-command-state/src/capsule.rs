//! Capsules execute once with merged output, independently of companion ledger publication.
use crate::{Result, error};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// A no-ledger capture needs the same child ownership and cancellation protocol
/// as native runs, without entering the ledger-producing execution adapter.
pub fn execute_merged(
    command: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    timeout: Option<Duration>,
    output: &mut dyn Write,
) -> Result<i32> {
    let context = oms_command_runs::Context::new(cwd, interrupted).map_err(|e| error(e.code, e))?;
    let result = oms_command_runs::process::merged(&context, command, timeout, output)
        .map_err(|e| error(e.code, e))?;
    Ok(result.code)
}
