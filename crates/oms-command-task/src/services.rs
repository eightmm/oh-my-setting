//! In-process service boundary. Implementors are supplied by the parent CLI;
//! no missing service is replaced with a legacy process or fabricated success.
use crate::{Error, Result};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// An actual foreground reference operation with native generation/heartbeat
/// ownership. Drop must stop its owned monitor, leaving uncertain operations
/// unresolved. `finish` must preserve an already terminal no-write refusal.
pub trait ReferenceOperation {
    /// Actual native reference-operation attempt, when the owner has one.
    /// This is not an inferred worker, Lead, or environment-supplied attempt.
    fn attempt_id(&self) -> Option<&str> {
        None
    }
    /// Called under the canonical packet lock, before publication. Validate old
    /// AND proposed bytes plus all arguments against live managed references;
    /// preserve landing-capture ownership/index checks and namespace guards.
    fn validate(&mut self, request: &ReferenceWrite<'_>) -> Result<()>;
    fn finish(&mut self, exit: i32) -> Result<()>;
}
pub struct ReferenceWrite<'a> {
    pub repo: &'a Path,
    pub target: &'a Path,
    pub operation: &'a str,
    pub arguments: &'a [Vec<u8>],
    pub before: &'a [u8],
    pub proposed: &'a [u8],
    pub source_session: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalOperation {
    Update,
    Verify,
    Close,
}
impl JournalOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Verify => "verify",
            Self::Close => "close",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationStatus {
    Passed,
    NotVerified,
}
pub struct JournalObservation<'a> {
    pub repo: &'a Path,
    /// Current packet, or the actual archived path for close.
    pub path: &'a Path,
    pub operation: JournalOperation,
    /// Only close supplies the pre-archive freshness result; the native journal
    /// producer derives update/verify status from the packet itself.
    pub verification: Option<VerificationStatus>,
}
pub struct MemoryOutcome<'a> {
    pub repo: &'a Path,
    pub agent: &'a [u8],
    pub text: &'a [u8],
    /// Canonical memory producer's source tag, always `task_close`.
    pub source: &'static str,
    pub task_id: &'a [u8],
    /// Freshness observed at close, before the archive changes packet status.
    pub verification: VerificationStatus,
}
pub struct FailureObservation<'a> {
    pub repo: &'a Path,
    pub command: &'a [u8],
    pub exit: i32,
    pub summary: &'a [u8],
    pub verification: Option<&'a VerificationEvidence>,
}

/// Bytes and native reference identities actually published by this verify.
/// Only the task producer constructs this evidence, after publication and its
/// reference finish succeed. An accepted historical receipt is not authority
/// for a later tree, environment, task or memory promotion.
pub struct VerificationEvidence {
    pub(crate) active_task_id: Vec<u8>,
    pub(crate) snapshot_attempt_id: Option<String>,
    pub(crate) snapshot_sha256: String,
    pub(crate) outcome_attempt_id: Option<String>,
    pub(crate) outcome_sha256: String,
    pub(crate) command_sha256: String,
    pub(crate) verified_state: Option<String>,
    pub(crate) prior_failures: Option<Vec<String>>,
}
impl VerificationEvidence {
    pub fn active_task_id(&self) -> &[u8] {
        &self.active_task_id
    }
    pub fn snapshot_attempt_id(&self) -> Option<&str> {
        self.snapshot_attempt_id.as_deref()
    }
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }
    pub fn outcome_attempt_id(&self) -> Option<&str> {
        self.outcome_attempt_id.as_deref()
    }
    pub fn outcome_sha256(&self) -> &str {
        &self.outcome_sha256
    }
    pub fn command_sha256(&self) -> &str {
        &self.command_sha256
    }
    pub fn verified_state(&self) -> Option<&str> {
        self.verified_state.as_deref()
    }
    /// Exact failure row hashes observed before this command ran. `None`
    /// means the source was unavailable, invalid or over its selection bound.
    pub fn prior_failures(&self) -> Option<&[String]> {
        self.prior_failures.as_deref()
    }
}
#[derive(Default)]
pub struct VerifyObservation {
    pub lines: Vec<u8>,
    pub zero_runners: Vec<String>,
}

pub trait Services {
    /// Resolve oms_state_root for reference ownership, including validated
    /// parent scratch worktrees. `work` is a fresh physical Git/directory root.
    fn state_repository(&mut self, work: &Path, interrupted: &AtomicBool) -> Result<PathBuf>;
    /// Start the real local agent-task reference operation before acquiring its
    /// packet lock. The returned object owns its lifetime, including failure.
    fn begin_reference(
        &mut self,
        repo: &Path,
        target: &Path,
    ) -> Result<Box<dyn ReferenceOperation>>;
    /// Wire to oms-command-journal's native source observer; it owns all event
    /// validation, dedupe, locks, JSONL writes and daily projections.
    fn observe_journal(&mut self, request: JournalObservation<'_>) -> Result<()>;
    /// Wire to oms-command-memory's project append API, not a second writer.
    fn promote_memory(&mut self, request: MemoryOutcome<'_>) -> Result<()>;
    /// Snapshot task/command-scoped failure identities before running verify;
    /// unknown is not an empty set, and timing never establishes causality.
    fn failure_candidates(
        &mut self,
        _repo: &Path,
        _active_task_id: &[u8],
        _command: &[u8],
    ) -> Result<Vec<String>> {
        Err(Error::service("failure correlation"))
    }
    /// Record a failure or an accepted rerun. The ledger owns the distinction
    /// between exact recovery evidence and command-wide aggregate resolution.
    /// Return only advisory text intended for stderr (e.g. `oms advise`).
    fn observe_failure(&mut self, request: FailureObservation<'_>) -> Result<Vec<u8>>;
    /// Advisory native verify_observe equivalent; errors drop advisory lines.
    fn observe_verify(&mut self, repo: &Path, output: &[u8]) -> Result<VerifyObservation>;
    /// Read-only skill-forge contract hints. Never execute these commands.
    fn skill_contracts(&mut self, repo: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

pub struct Unavailable;
impl Services for Unavailable {
    fn state_repository(&mut self, _: &Path, _: &AtomicBool) -> Result<PathBuf> {
        Err(Error::service("state repository"))
    }
    fn begin_reference(&mut self, _: &Path, _: &Path) -> Result<Box<dyn ReferenceOperation>> {
        Err(Error::service("reference operation"))
    }
    fn observe_journal(&mut self, _: JournalObservation<'_>) -> Result<()> {
        Err(Error::service("journal"))
    }
    fn promote_memory(&mut self, _: MemoryOutcome<'_>) -> Result<()> {
        Err(Error::service("memory"))
    }
    fn observe_failure(&mut self, _: FailureObservation<'_>) -> Result<Vec<u8>> {
        Err(Error::service("failure ledger"))
    }
    fn observe_verify(&mut self, _: &Path, _: &[u8]) -> Result<VerifyObservation> {
        Err(Error::service("verify observations"))
    }
    fn skill_contracts(&mut self, _: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Err(Error::service("skill contracts"))
    }
}
