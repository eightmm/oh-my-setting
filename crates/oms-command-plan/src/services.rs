//! Narrow adapters to native producer domains; no legacy command invocation.
use crate::json::Value;
use crate::{Result, error};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The guard owns the actual agent-events writer primitive, not a plan lock.
pub trait LifecycleGuard {}
pub struct ProjectedLifecycle {
    pub source_sha256: String,
    /// Exact `agent-events.project_attempts` object, including unknown refs.
    pub attempts: Value,
}
pub struct ProjectedRoom {
    pub source_sha256: String,
    /// Exact `room.project` schema-1 projection; the plan checks membership epochs.
    pub state: Value,
}
pub struct SupervisedCommand {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub wall: Duration,
    pub output_limit: usize,
    pub label: &'static str,
    /// Remove retirement authority from the entire acceptance command tree.
    pub remove_environment: Vec<&'static str>,
    pub environment: Vec<(OsString, OsString)>,
}
pub struct Execution {
    pub exit: u8,
    pub runner_exit: u8,
    pub timed_out: bool,
    pub output_limited: bool,
    pub launch_error: bool,
    pub supervision_error: bool,
    pub output: Vec<u8>,
    pub duration_s: u64,
}
pub struct MainReviewerRequest<'a> {
    pub state_repo: &'a Path,
    pub execution_repo: &'a Path,
    pub task_id: &'a str,
    pub bundle_sha256: &'a str,
    pub room_id: &'a str,
    pub participant: &'a str,
    pub approval_ref: &'a Value,
    /// Exact main-read-completion-v1 request including approved marker/argv.
    pub request: &'a Value,
}
pub struct CapturedMainReviewer {
    /// Schema-2 `main_reviewer.capture_completed_main_reviewer` proof bytes.
    pub raw: Vec<u8>,
}
pub struct ApprovalGrant {
    /// Exact `approval-inbox show --json` public projection.
    pub public_record: Vec<u8>,
    /// Complete validated native private approval stream, never persisted by plan.
    pub private_rows: Vec<u8>,
}
/// Fresh native account-database result for one group. A primary gid alone is
/// insufficient: both the explicit member accounts and every primary member
/// must be resolved by the actual host account provider.
pub struct GroupMembership {
    pub gid: u32,
    pub explicit_member_uids: Vec<u32>,
    pub primary_member_uids: Vec<u32>,
}
pub type GroupResolver = fn(u32) -> Result<GroupMembership>;

pub trait Services {
    /// Verified shared state root for a managed scratch worktree of `repo`
    /// (the `oms_state_root` contract). `None` keeps the repository's own root.
    /// Consulted only when `OMS_STATE_REPO` is unset; never inferred here.
    fn state_root(&self, _repo: &Path) -> Result<Option<PathBuf>> {
        Ok(None)
    }
    fn group_membership_resolver(&self) -> Option<GroupResolver> {
        None
    }
    fn lifecycle_writer(&self, _repo: &Path) -> Result<Box<dyn LifecycleGuard + '_>> {
        Err(error("native lifecycle writer service unavailable"))
    }
    fn project_lifecycle(&self, _raw: &[u8]) -> Result<ProjectedLifecycle> {
        Err(error("native lifecycle projection service unavailable"))
    }
    fn project_room(&self, _room: &str, _raw: &[u8]) -> Result<ProjectedRoom> {
        Err(error("native room projection service unavailable"))
    }
    fn supervise(&self, _command: &SupervisedCommand) -> Result<Execution> {
        Err(error(
            "native whole-tree command supervision service unavailable",
        ))
    }
    fn capture_main_reviewer(
        &self,
        _request: &MainReviewerRequest<'_>,
    ) -> Result<CapturedMainReviewer> {
        Err(error(
            "native completed main-review approval service unavailable",
        ))
    }
    fn landing_approval(&self, _repo: &Path, _approval_id: &str) -> Result<ApprovalGrant> {
        Err(error("native private landing approval service unavailable"))
    }
    fn normalize_provider(&self, provider: &str) -> Result<String> {
        match provider {
            "codex" | "claude" | "antigravity" => Ok(provider.into()),
            "agy" => Ok("antigravity".into()),
            _ => Err(error(
                "native provider registry service unavailable for this transport",
            )),
        }
    }
    fn verify_selector_warnings(&self, _repo: &Path, _command: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
    /// Shared panel results producer proves exact patch tree and publication.
    /// Domain code independently verifies task/plan/lease/hash/review bindings.
    fn publication_product(
        &self,
        _repo: &Path,
        _commit: &str,
        _record: &Value,
        _patch: &[u8],
    ) -> Result<PublicationProduct> {
        Err(error(
            "native panel publication product service unavailable",
        ))
    }
    /// Native controller source bytes. The caller identifies only its executing
    /// binary/source, never an arbitrary reviewed manifest supplied by a bundle.
    fn controller_manifest(&self, _repo: &Path) -> Result<Value> {
        Err(error(
            "native executing controller manifest service unavailable",
        ))
    }
    /// Native acceptance producer's context includes raw tracked bytes, environment,
    /// ordered Git config and executing controller identity. Empty disables reuse.
    fn acceptance_reuse_context(&self, _repo: &Path) -> Result<Option<String>> {
        Ok(None)
    }
    /// Verify and read a managed `.oms/landing-patches/` capture (whose
    /// publication anchor may hold a validated 2-link identity).
    fn read_managed_patch(
        &self,
        _state_repo: &Path,
        _path: &Path,
        _limit: usize,
    ) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}
pub struct PublicationProduct {
    pub commit: String,
    pub patch_sha256: String,
    pub accepted_tree: String,
    pub base: String,
    pub raw_proof: Vec<u8>,
}
pub struct UnavailableServices;
impl Services for UnavailableServices {}
