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
