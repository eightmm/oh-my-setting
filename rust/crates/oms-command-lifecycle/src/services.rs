use crate::Output;
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};

/// Native domains owned by the embedding dispatcher. Implementations must call
/// native producers with their normal locks and authority checks, and must
/// preserve cancellation. An unavailable service never counts as a clean sweep.
pub trait Services {
    /// Explicit service/permission/publication options belong to their native
    /// producers. Preflight is read-only; Reconcile runs only after core commit
    /// and retains each producer's independent journal and authority checks.
    fn install_extras(
        &self,
        _phase: IncomingPhase,
        _source: &Path,
        _args: &[OsString],
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native install extras host routing unavailable")
    }
    /// Read-only proof that the destination requires the stable CI gate.
    fn hook_protection(&self, _source: &Path, _interrupted: &AtomicBool) -> Output {
        unavailable("native CI protection host routing unavailable")
    }

    /// Native project-template producer for isolated doctor contract fixtures.
    /// The destination is disposable; implementations must not install globally.
    fn apply_project_template(
        &self,
        _style: &str,
        _destination: &Path,
        _source: &Path,
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native project-template host routing unavailable")
    }
    /// Native hook domain, supplied bytes only. Advisory calls cannot block an
    /// Antigravity turn; the lifecycle adapter owns provider payload translation.
    fn provider_hook(
        &self,
        _name: &str,
        _input: &[u8],
        _cwd: &Path,
        _source: &Path,
        _advisory: bool,
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native hook host routing unavailable")
    }

    /// Native incoming-checkout handoff. Preflight must prove the host can build
    /// and dispatch the selected checkout before any Git mutation. Reconcile
    /// validates that checkout's own payload and converges recorded components,
    /// returning its schema-2 install receipt as JSON. Verify runs its doctor.
    /// RefreshTools runs after the core transaction commits. Implementations
    /// launch the incoming native --oms-dispatch frontdoor with the child-only
    /// OMS_NATIVE_UPDATE_PARENT environment set to the updater PID. The child
    /// verifies the lease, live PID generation, ancestry and held receipt mutex
    /// before borrowing that mutex; its component journal remains independent.
    /// Reconcile/Verify must finish every child before returning. No shell or
    /// Python lifecycle producer is an acceptable implementation.
    fn incoming_lifecycle(
        &self,
        _phase: IncomingPhase,
        _source: &Path,
        _revision: &str,
        _receipt: &[u8],
        _args: &[OsString],
        _interrupted: &AtomicBool,
    ) -> Output {
        unavailable("native incoming lifecycle gate unavailable; host wiring required")
    }

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
    /// Resolves group membership when an existing repository directory carries
    /// group-write (`0o020`) permissions (e.g. UPG developer checkouts).
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        None
    }
}
pub struct UnavailableServices;
impl Services for UnavailableServices {}
fn unavailable(message: &str) -> Output {
    (2, Vec::new(), format!("error: {message}\n").into_bytes())
}

/// Explicit host operations, with no inference of authority from provider data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingPhase {
    Preflight,
    Reconcile,
    Verify,
    RefreshTools,
}
