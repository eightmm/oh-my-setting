use crate::Output;
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};

/// Native domains owned by the embedding dispatcher. Implementations must call
/// native producers with their normal locks and authority checks, and must
/// preserve cancellation. An unavailable service never counts as a clean sweep.
pub trait Services {
    /// Return the canonical journal/retirement/artifact/failure phase receipt.
    /// Required fields are documented in the crate README. No provider, shell,
    /// or Python bridge may be selected as a substitute for a native producer.
    fn tick_maintenance(&self, _repo: &Path, _source: &Path, _interrupted: &AtomicBool) -> Output {
        unavailable("native tick maintenance service unavailable")
    }
    /// The native scheduler must validate the approved contract and preserve its
    /// journal/approval binding; registration or a path alone grants no approval.
    fn tick_schedule(
        &self,
        _args: &[OsString],
        _repo: &Path,
        _source: &Path,
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native approved-contract schedule service unavailable")
    }
    fn refresh_catalogs(
        &self,
        _dry_run: bool,
        _source: &Path,
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native provider catalog refresh service unavailable")
    }
    fn garbage_collect(&self, _repo: &Path, _interrupted: &AtomicBool) -> Output {
        unavailable("native optional garbage collection service unavailable")
    }
    /// Returns the canonical wake receipt, including its preserved daily ledger.
    /// The implementation owns idle/empty-input/approved-schedule checks and the
    /// existing wake budget. A sweep must never reset that ledger.
    fn wake_idle_main(
        &self,
        _scheduled_only: bool,
        _repo: &Path,
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native idle-main notification service unavailable")
    }
}
pub struct UnavailableServices;
impl Services for UnavailableServices {}
fn unavailable(message: &str) -> Output {
    (2, Vec::new(), format!("error: {message}\n").into_bytes())
}
