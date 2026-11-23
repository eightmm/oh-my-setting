//! Typed authority boundaries for composition by the native `oms` binary.
pub use crate::git::Execution;
pub use crate::git::Snapshot;
use crate::{Error, Result, V};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub struct VerifyRequest {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub wall: Duration,
    pub environment: Vec<(OsString, OsString)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    Passed,
    Failed,
    Cancelled,
    TimedOut,
    OutputLimited,
    Unavailable,
}
impl VerificationOutcome {
    pub(crate) fn status(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::OutputLimited => "output_limited",
            Self::Unavailable => "unavailable",
        }
    }
    pub(crate) fn exit(self) -> i32 {
        match self {
            Self::Passed => 0,
            Self::Failed => 1,
            Self::Cancelled => 130,
            Self::TimedOut => 124,
            Self::OutputLimited => 125,
            Self::Unavailable => 2,
        }
    }
}
pub struct BatchVerification {
    pub outcome: VerificationOutcome,
    pub verifier_exit: Option<i32>,
    pub elapsed: Duration,
}
#[derive(Clone, Debug)]
pub struct PublicationScan {
    pub repo: PathBuf,
    pub base: String,
    pub head: String,
    pub wall: Duration,
    pub max_objects: u64,
    pub max_object_bytes: u64,
    pub max_total_bytes: u64,
    pub memory_bytes: u64,
}
pub struct LandingRequest<'a> {
    pub state_repo: &'a Path,
    pub execution_repo: &'a Path,
    pub patch_sha256: &'a str,
    pub base: &'a str,
    pub task: &'a str,
    pub admission: &'a V,
    pub policy: &'a crate::admission::Policy,
    /// Published landing patch the intent records; empty before admission.
    /// A task-bound done receipt binds this exact path.
    pub frozen_patch: &'a Path,
    /// Admitted patch bytes hashing to `patch_sha256`; empty in recovery,
    /// which replays receipts and never re-reviews.
    pub patch: &'a [u8],
}
pub trait PublicationGuard {}
/// A production adapter must bind these checks to native lifecycle, immutable
/// reviewer/approval captures, authenticated dispatch grants and lease CAS.
/// Tool output, environment labels and room membership are never grants.
pub trait Services {
    fn plan(&self) -> Option<&dyn oms_command_plan::Services> {
        None
    }
    fn admission_context(&self) -> Option<&dyn oms_reference::binding::Context> {
        None
    }
    fn controller(&self) -> Result<oms_reference::controller::Controller> {
        Ok(oms_reference::controller::Controller::capture_current()?)
    }
    fn supervise(&self, _request: &VerifyRequest, _interrupted: &AtomicBool) -> Result<Execution> {
        Err(Error::unavailable("whole-tree verification"))
    }
    /// Run an acknowledged batch candidate with network disabled and only the
    /// private copied checkout writable. The ordinary landing gate has a
    /// different environment contract and cannot substitute for this method.
    fn supervise_batch(
        &self,
        _request: &VerifyRequest,
        _interrupted: &AtomicBool,
    ) -> Result<BatchVerification> {
        Err(Error::unavailable("isolated batch verification"))
    }
    /// Invoke `review::scan_worker` in a dedicated native process, retaining
    /// whole-tree timeout/cancellation/output enforcement and the exact request.
    /// Never call the worker inside the embedding process: it lowers RLIMIT_AS.
    fn publication_scan(
        &self,
        _request: &PublicationScan,
        _interrupted: &AtomicBool,
    ) -> Result<()> {
        Err(Error::unavailable("bounded native publication scan worker"))
    }
    fn publication_guard(
        &self,
        _repo: &Path,
        _interrupted: &AtomicBool,
    ) -> Result<Box<dyn PublicationGuard + '_>> {
        Err(Error::unavailable("canonical publication fence"))
    }
    fn landing_review(&self, _request: &LandingRequest<'_>) -> Result<V> {
        Err(Error::unavailable("main reviewer and peer landing review"))
    }
    /// Revalidate the exact panel task, candidate and current owner. The JSON
    /// supplied by a caller is a comparison target, never an authority grant.
    fn panel_accept(&self, _request: &LandingRequest<'_>, _fence: &V) -> Result<()> {
        Err(Error::unavailable("panel acceptance authority"))
    }
    fn fence_landing(&self, _request: &LandingRequest<'_>, _intent: &V) -> Result<()> {
        Err(Error::unavailable(
            "landing lease and approval compare-and-set",
        ))
    }
    fn recover_landing(
        &self,
        _request: &LandingRequest<'_>,
        _intent: &V,
        _applied: bool,
    ) -> Result<()> {
        Err(Error::unavailable(
            "landing recovery lineage and lease reconciliation",
        ))
    }
    fn landing_approval(
        &self,
        _request: &LandingRequest<'_>,
        _approval: &str,
        _grant: &str,
        _version: &str,
        _request_only: bool,
    ) -> Result<V> {
        Err(Error::unavailable("private landing approval"))
    }
    fn complete_landing(&self, _request: &LandingRequest<'_>, _receipt: &V) -> Result<()> {
        Err(Error::unavailable("plan landing completion"))
    }
    fn coverage(&self, _repo: &Path, _ids: &[String], _evidence: Option<&V>) -> Result<V> {
        Err(Error::unavailable("runtime criterion coverage"))
    }
    fn cleanup(&self, _repo: &Path, _apply: bool, _interrupted: &AtomicBool) -> Result<V> {
        Err(Error::unavailable("install and residue cleanup"))
    }
    fn wait_siblings(
        &self,
        _repo: &Path,
        _budget: Duration,
        _interrupted: &AtomicBool,
    ) -> Result<()> {
        Err(Error::unavailable("live sibling worktree observer"))
    }
    fn after_land(&self, _repo: &Path, _receipt: &V, _interrupted: &AtomicBool) -> Result<V> {
        Err(Error::unavailable("CI-authorized install and handoff"))
    }
    fn failure_report(&self, _repo: &Path, _fingerprint: &str) -> Result<V> {
        Err(Error::unavailable("allowlisted failure report"))
    }
    fn review_findings(&self, _repo: &Path) -> Result<V> {
        Err(Error::unavailable("peer review findings"))
    }
    fn ci_comparison(
        &self,
        repo: &Path,
        branch: &str,
        sha: &str,
        interrupted: &AtomicBool,
    ) -> Result<V> {
        crate::ci::comparison(repo, branch, sha, interrupted)
    }
    fn check_python_syntax(&self, _path: &Path, _source: &[u8]) -> Option<bool> {
        None
    }
}
pub struct Unavailable;
impl Services for Unavailable {}
