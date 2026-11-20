//! Independent producer boundaries. Missing producers refuse; no legacy dispatch.
use crate::value::Value;
use crate::{Result, unavailable};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RuntimeRoots {
    pub execution_repo: PathBuf,
    pub state_repo: PathBuf,
}
#[derive(Debug, Clone)]
pub struct RepositoryView {
    pub head: String,
    pub branch: String,
    pub worktree_state: String,
}
#[derive(Debug, Clone)]
pub struct BackendRequest {
    pub profile: String,
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub worktree: Option<PathBuf>,
    pub image: OsString,
    pub adapter: OsString,
    pub allow_network: bool,
    pub inherit_env: Vec<OsString>,
    pub wall: Duration,
    pub log: Option<PathBuf>,
    pub receipt: Option<PathBuf>,
    pub log_limit: usize,
    pub memory_mb: u64,
    pub cpus: f64,
    pub pids: u64,
    pub tmpfs_mb: u64,
}
#[derive(Debug, Clone)]
pub struct BackendResult {
    pub receipt: Value,
    pub exit: i32,
}
#[derive(Debug, Clone)]
pub struct InstallerRequest {
    pub profiles: Vec<String>,
    pub primary_provider: Option<String>,
    pub upgrade: bool,
    pub allow_missing: bool,
    pub receipt: Option<PathBuf>,
    pub dry_run: bool,
}
#[derive(Debug, Clone)]
pub struct ExternalOutput {
    pub exit: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct ReleaseRequest {
    pub channel: String,
    pub target: String,
    /// Preserve the resolved ref's namespace alongside its exact target.
    pub selected_ref: String,
    pub no_tools: bool,
}
#[derive(Debug, Clone)]
pub struct PromotionRequest {
    pub commit: String,
    pub version: String,
    pub expected_manifest_digest: String,
    pub manifest: Value,
}
#[derive(Debug, Clone)]
pub struct TargetRequest {
    pub name: String,
    pub ssh_alias: String,
    pub workdir: String,
    pub sync: bool,
    pub sha: Option<String>,
    pub timeout: Duration,
}
#[derive(Debug, Clone)]
pub struct LedgerRequest {
    pub operation_id: String,
    pub argv: Vec<OsString>,
    pub metrics: Option<PathBuf>,
    pub file: Option<PathBuf>,
    pub note: String,
    pub research: Value,
    pub no_gate: bool,
    pub reason: String,
    pub wall: Duration,
}
#[derive(Debug, Clone)]
pub struct LedgerResult {
    pub output: ExternalOutput,
    pub ledger_id: String,
}

#[derive(Debug, Clone)]
pub struct RelatedSource {
    pub path: PathBuf,
    pub reason: String,
    pub priority: i32,
    pub source_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceAction {
    Host,
    Status,
    Activate { policy: PathBuf },
    Hold { expected_policy_sha256: String },
}

pub trait Services {
    /// Native resource producer owns policy validation, actual host generation,
    /// exact state-root custody and dispatch CAS. No execution permit is returned.
    fn resource_action(
        &self,
        _repo: &Path,
        _state_repo: &Path,
        _action: &ResourceAction,
    ) -> Result<Value> {
        Err(unavailable("resource policy"))
    }
    /// Read-only native account-database lookup for an explicitly named ~user.
    /// This supplies metadata, with no file access or execution authority.
    fn account_home(&self, _username: &std::ffi::OsStr) -> Result<Option<PathBuf>> {
        Err(unavailable("account home directory"))
    }
    /// Resolve the raw user path with native tilde/Git-root/shared-state semantics.
    fn resolve_roots(&self, _cwd: &Path, _requested: &std::ffi::OsStr) -> Result<RuntimeRoots> {
        Err(unavailable("repository root resolution"))
    }
    /// Native graph/discovery producer: validate graph freshness and coverage,
    /// then native Python AST fallback for every unresolved/uncertain target.
    /// Return source anchors, never an assembled runtime context response.
    fn context_dependencies(
        &self,
        _repo: &Path,
        _targets: &[PathBuf],
    ) -> Result<Vec<RelatedSource>> {
        Err(unavailable("context dependency discovery"))
    }
    fn group_resolver(&self) -> Option<oms_command_plan::services::GroupResolver> {
        None
    }
    /// One immutable native Git view per command, with the task verifier's exact fingerprint.
    fn repository_view(&self, _repo: &Path) -> Result<RepositoryView> {
        Err(unavailable("repository snapshot"))
    }
    /// Exact canonical schema-1 task status for these captured packet bytes.
    fn task_status(&self, _repo: &Path, _state_repo: &Path, _packet: &[u8]) -> Result<Vec<u8>> {
        Err(unavailable("task status"))
    }
    /// Native plan status must verify real lifecycle, room, leases and approvals.
    fn plan_status(&self, _repo: &Path, _state_repo: &Path, _plan: &[u8]) -> Result<Vec<u8>> {
        Err(unavailable("verified plan status"))
    }
    fn ensure_plan_lineage(&self, _repo: &Path, _state_repo: &Path) -> Result<()> {
        Err(unavailable("plan lineage"))
    }
    fn unresolved_failures(
        &self,
        _repo: &Path,
        _state_repo: &Path,
        _ledger: &[u8],
    ) -> Result<Vec<u8>> {
        Err(unavailable("failure ledger"))
    }
    fn backend_check(
        &self,
        _profile: &str,
        _image: &std::ffi::OsStr,
        _adapter: &std::ffi::OsStr,
    ) -> Result<Value> {
        Err(unavailable("backend readiness"))
    }
    /// Own the unreaped leader through group and pipe cleanup; no PID-only authority,
    /// scoped blocking joins, post-reap killpg or arbitrary post-exit pipe deadline.
    /// Preserve canonical durable log/receipt, remote attestations and exact exit.
    fn supervise(&self, _repo: &Path, _request: &BackendRequest) -> Result<BackendResult> {
        Err(unavailable("whole-tree supervisor"))
    }
    fn install_profiles(
        &self,
        _repo: &Path,
        _request: &InstallerRequest,
    ) -> Result<ExternalOutput> {
        Err(unavailable("profile installer"))
    }
    fn resolve_revision(&self, _root: &Path, _revision: &str) -> Result<Option<String>> {
        Err(unavailable("local revision resolution"))
    }
    fn fetch_release(&self, _root: &Path) -> Result<()> {
        Err(unavailable("release fetch"))
    }
    fn apply_release(&self, _root: &Path, _request: &ReleaseRequest) -> Result<ExternalOutput> {
        Err(unavailable("release publication"))
    }
    /// Enforce clean tracked tree, exact source SHA, lock, reviewed manifest CAS and fsync.
    fn promote_release(&self, _root: &Path, _request: &PromotionRequest) -> Result<Value> {
        Err(unavailable("release promotion"))
    }
    /// Transport implements canonical identity/branch/origin/path validation, no runner.
    fn target_transport(&self, _repo: &Path, _request: &TargetRequest) -> Result<Value> {
        Err(unavailable("SSH target transport"))
    }
    /// Canonical native ledger owns command execution, verification, memory and journal writes.
    fn run_ledger(&self, _repo: &Path, _request: &LedgerRequest) -> Result<LedgerResult> {
        Err(unavailable("run ledger"))
    }
    fn reservation_process_gone(&self, _host: &str, _pid: u32) -> Result<bool> {
        Err(unavailable("reservation process identity"))
    }
    fn host_name(&self) -> Result<String> {
        Err(unavailable("local host identity"))
    }
    fn experiment_gpu_usage(&self, _repo: &Path, _id: &str) -> Result<Value> {
        Err(unavailable("experiment run accounting"))
    }
}
pub struct Unavailable;
impl Services for Unavailable {}
