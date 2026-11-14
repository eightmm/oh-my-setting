//! Narrow requests to lifecycle owners. These types carry no write authority.
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerOperation {
    StageNativePayload,
    WriteInstallReceipt,
    MaterializeFrontdoor,
}
#[derive(Debug)]
pub struct ProducerRequest {
    pub operation: ProducerOperation,
    pub source_root: PathBuf,
    pub source_revision: String,
    pub destination: PathBuf,
}

/// An in-memory acknowledgment from the real producer after its atomic
/// publication. This is not a persisted receipt schema or a dispatch proof.
#[derive(Debug)]
pub struct ProducerReceipt {
    pub operation: ProducerOperation,
    pub source_root: PathBuf,
    pub source_revision: String,
    pub destination: PathBuf,
    pub content_digest: String,
}
impl ProducerReceipt {
    pub fn matches(&self, request: &ProducerRequest) -> bool {
        self.operation == request.operation
            && self.source_root == request.source_root
            && self.source_revision == request.source_revision
            && self.destination == request.destination
            && self.source_root.is_absolute()
            && self.destination.is_absolute()
            && crate::hex(self.source_revision.as_bytes(), 40)
            && crate::hex(self.content_digest.as_bytes(), 64)
    }
}
#[derive(Debug)]
pub struct ProducerUnavailable {
    pub operation: ProducerOperation,
}
impl fmt::Display for ProducerUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "native bootstrap producer {:?} is unavailable; the lifecycle owner must provide a reviewed implementation",
            self.operation
        )
    }
}
impl std::error::Error for ProducerUnavailable {}
impl ProducerRequest {
    /// Always unavailable in this phase. Never starts shell lifecycle writers.
    pub fn execute(self) -> Result<std::convert::Infallible, ProducerUnavailable> {
        Err(ProducerUnavailable {
            operation: self.operation,
        })
    }
}
