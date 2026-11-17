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
    /// Record a failing verify or resolve this exact command after a pass.
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
