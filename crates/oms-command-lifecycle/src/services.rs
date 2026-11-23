use crate::Output;
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};

/// A parent-owned managed checkout for one exact incoming revision. A path or
/// environment value alone cannot replace its marker and Git registration proof.
pub trait IncomingSourceLease {
    fn source_root(&self) -> &Path;
    fn recheck(&self, interrupted: &AtomicBool) -> std::result::Result<(), Output>;
    /// Explicit cleanup reports failure and preserves unproven ownership/work.
    fn close(self: Box<Self>, interrupted: &AtomicBool) -> Output;
}

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

    /// Prepare an exact managed incoming checkout without changing the live
    /// checkout, local refs or installed targets. Core owns build/publication.
    fn prepare_incoming_source(
        &self,
        _source: &Path,
        _revision: &str,
        _interrupted: &AtomicBool,
    ) -> std::result::Result<Box<dyn IncomingSourceLease + '_>, Output> {
        Err(unavailable(
            "native incoming source preparation unavailable",
        ))
    }

    /// Native incoming-checkout handoff. Preflight proves the prepared payload
    /// is dispatchable before live checkout/local-ref/install mutation. Reconcile
    /// validates that checkout's own payload and converges recorded components,
    /// returning its schema-2 install receipt as JSON. Verify runs its doctor.
    /// RefreshTools and VerifyIntegrations run after the core transaction commits.
    /// The latter checks recorded integrations after tool refresh, without live
    /// model inference or arbitrary MCP server probes. Post-commit operations
    /// return only an IncomingDiagnostic; child output is withheld. Implementations
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncomingPhase {
    Preflight,
    Reconcile,
    Verify,
    RefreshTools,
    VerifyIntegrations,
}

/// Fixed command names only. Provider output and executable paths must never
/// become update diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomingTool {
    #[serde(rename = "install-tools")]
    InstallTools,
    #[serde(rename = "install-profile")]
    InstallProfile,
    #[serde(rename = "doctor")]
    Doctor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingDiagnostic {
    pub phase: IncomingPhase,
    /// None means the host could not establish which command was launched.
    pub tool: Option<IncomingTool>,
    pub exit: i32,
}
impl IncomingDiagnostic {
    pub fn output(self) -> Output {
        (
            self.exit,
            serde_json::to_vec(&self).expect("fixed diagnostic fields serialize"),
            Vec::new(),
        )
    }
    pub(crate) fn valid(&self) -> bool {
        (0..=255).contains(&self.exit)
            && matches!(
                (self.phase, self.tool),
                (
                    IncomingPhase::RefreshTools,
                    None | Some(IncomingTool::InstallTools | IncomingTool::InstallProfile)
                ) | (
                    IncomingPhase::VerifyIntegrations,
                    None | Some(IncomingTool::Doctor)
                )
            )
    }
    pub(crate) fn capture(phase: IncomingPhase, out: &Output) -> Self {
        // Hosts may predate the diagnostic contract or fail before launching.
        // Keep their exit status, but never interpret raw output as a tool name.
        (out.1.len() <= 1024)
            .then(|| serde_json::from_slice::<Self>(&out.1).ok())
            .flatten()
            .filter(|d| d.phase == phase && d.exit == out.0 && d.valid())
            .unwrap_or(Self {
                phase,
                tool: None,
                exit: if (0..=255).contains(&out.0) { out.0 } else { 2 },
            })
    }
}
