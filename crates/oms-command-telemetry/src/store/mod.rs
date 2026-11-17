//! Canonical, repository-confined artifact-index transactions.
//!
//! Writers use Bash-compatible lock selection, held across payload hashing,
//! retention and publication. The CLI reuses these transaction primitives.
//! A publication-phase storage error requires reconciliation before retrying.
#[path = "../actions.rs"]
pub(crate) mod actions;
pub mod budget;
pub mod dispatch;
mod paths;
mod producer;
mod recovery;
mod retention;
pub mod usage;
#[path = "../waste.rs"]
pub(crate) mod waste;

pub use producer::{AppendArtifactRequest, append_artifact_index, register_panel_result};
pub use recovery::{OrphanReport, SalvageReport, delete_orphans, salvage_index};
pub use retention::{
    CompletionReferences, RetentionOptions, completion_pins, completion_references,
    decision_pin_indices, retained_lines, retention_limits,
};

use crate::value::{V, parse};
use oms_safe_fs::{
    ByteError, ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAX_INDEX_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_ROW_BYTES: usize = 1024 * 1024;
pub const DEFAULT_INDEX: &str = ".oms/artifacts/index.jsonl";

/// Path-free errors preserve whether an atomic replacement may be visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Invalid(&'static str),
    Changed,
    Interrupted,
    Lock(oms_lock::Error),
    Storage {
        phase: MutationPhase,
        error: ByteError,
    },
    Io,
}
impl StoreError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Interrupted => 130,
            Self::Lock(error) => error.exit_code(),
            Self::Changed
            | Self::Storage {
                phase: MutationPhase::PublicationUncertain | MutationPhase::Published,
                ..
            } => 75,
            _ => 2,
        }
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "artifact index: {self:?}")
    }
}
impl std::error::Error for StoreError {}
impl From<ByteError> for StoreError {
    fn from(error: ByteError) -> Self {
        Self::Storage {
            phase: MutationPhase::BeforePublication,
            error,
        }
    }
}
impl From<MutationError<StoreError>> for StoreError {
    fn from(error: MutationError<StoreError>) -> Self {
        match error {
            MutationError::Transform(error) => error,
            MutationError::Storage { phase, error } => Self::Storage { phase, error },
        }
    }
}
impl From<oms_safe_fs::Error> for StoreError {
    fn from(_: oms_safe_fs::Error) -> Self {
        Self::Invalid("unsafe or unreadable repository path")
    }
}
impl From<rustix::io::Errno> for StoreError {
    fn from(error: rustix::io::Errno) -> Self {
        ByteError::Io(error).into()
    }
}
impl From<std::io::Error> for StoreError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppendOptions {
    pub keep: Option<i64>,
    pub high_water: Option<i64>,
    /// Exact bytes observed by the caller; compared under the writer lock.
    pub expected: Option<Vec<u8>>,
}

fn cancelled(interrupted: &AtomicBool) -> Result<(), StoreError> {
    if interrupted.load(Ordering::Acquire) {
        Err(StoreError::Interrupted)
    } else {
        Ok(())
    }
}
fn acquire(target: &Path, interrupted: &AtomicBool) -> Result<oms_lock::Guard, StoreError> {
    let mut settings = oms_lock::Settings::from_env().map_err(StoreError::Lock)?;
    settings.mode = oms_lock::Mode::LegacyAuto;
    let timeout = settings.timeout;
    let lock = oms_lock::Lock::for_file(target, settings).map_err(StoreError::Lock)?;
    let start = Instant::now();
    loop {
        cancelled(interrupted)?;
        match lock.try_acquire() {
            Ok(guard) => return Ok(guard),
            Err(oms_lock::Error::Busy) if start.elapsed() < timeout => {
                std::thread::sleep(
                    Duration::from_millis(50).min(timeout.saturating_sub(start.elapsed())),
                );
            }
            Err(error) => return Err(StoreError::Lock(error)),
        }
    }
}

/// Normalize logical-root aliases, then validate without following repo components.
/// Missing parents may be created as 0700; existing leaves must be owned,
/// single-link regular files. This helper alone does not serialize mutations.
pub fn canonical_index(
    repo: &Path,
    index: Option<&Path>,
    create_parent: bool,
    ensure_ignore: bool,
) -> Result<PathBuf, StoreError> {
    let root = Root::open(repo)?;
    let target = paths::target(&root, repo, index.unwrap_or(Path::new(DEFAULT_INDEX)))?;
    paths::validate(&root, &target, create_parent)?;
    if ensure_ignore {
        ensure_oms_ignore(root.physical_root())?;
    }
    Ok(target)
}

/// Initialize once under its own canonical lock; preserve any existing contents.
pub fn ensure_oms_ignore(repo: &Path) -> Result<(), StoreError> {
    ensure_ignore(repo, &AtomicBool::new(false))
}
fn ensure_ignore(repo: &Path, interrupted: &AtomicBool) -> Result<(), StoreError> {
    let root = Root::open(repo)?;
    let path = root.physical_root().join(".oms/.gitignore");
    let _guard = acquire(&path, interrupted)?;
    paths::validate(&root, &path, true)?;
    root.atomic_mutate(
        &path,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_INPUT_BYTES,
            },
            create_parent: true,
            preserve_existing: true,
            max_output: MAX_ROW_BYTES,
            ..MutationOptions::default()
        },
        |_| {
            cancelled(interrupted)?;
            Ok::<_, StoreError>(b"*\n".to_vec())
        },
    )?;
    paths::validate(&root, &path, false)?;
    Ok(())
}

/// Parse one finite-constant JSON object, accepting CRLF but no embedded CR.
/// Duplicate keys retain their last value, as in Python's index parser.
pub fn parse_index_line(line: &[u8]) -> Result<oms_context::Json, StoreError> {
    row(line)?;
    oms_context::parse_session_json(&line[..line.len() - 1])
        .map_err(|_| StoreError::Invalid("invalid JSON row"))
}
fn row(line: &[u8]) -> Result<V, StoreError> {
    if !line.ends_with(b"\n") {
        return Err(StoreError::Invalid("partial final JSONL row"));
    }
    if line.len() > MAX_ROW_BYTES {
        return Err(StoreError::Invalid("row exceeds byte limit"));
    }
    let payload = &line[..line.len() - 1];
    let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
    if payload.contains(&b'\r') || payload.contains(&b'\n') || payload.contains(&0) {
        return Err(StoreError::Invalid("row contains a forbidden control byte"));
    }
    let value = parse(payload, true).map_err(|_| StoreError::Invalid("invalid JSON row"))?;
    if !matches!(value, V::Object(_)) {
        return Err(StoreError::Invalid("row is not an object"));
    }
    Ok(value)
}
fn lines(body: &[u8]) -> Result<Vec<&[u8]>, StoreError> {
    if !body.is_empty() && !body.ends_with(b"\n") {
        return Err(StoreError::Invalid("partial final JSONL row"));
    }
    let source: Vec<_> = body.split_inclusive(|b| *b == b'\n').collect();
    for line in &source {
        row(line)?;
    }
    Ok(source)
}

pub fn read_index(
    repo: &Path,
    index: Option<&Path>,
    missing_ok: bool,
) -> Result<Vec<u8>, StoreError> {
    let root = Root::open(repo)?;
    let target = canonical_index(repo, index, false, false)?;
    let body = root.read_no_follow(
        &target,
        ByteReadOptions {
            missing_ok,
            max_bytes: MAX_INPUT_BYTES,
        },
    )?;
    paths::validate(&root, &target, false)?;
    lines(&body)?;
    Ok(body)
}

fn mutate(
    root: &Root,
    target: &Path,
    missing_ok: bool,
    expected: Option<&[u8]>,
    interrupted: &AtomicBool,
    transform: impl FnOnce(&[u8]) -> Result<Vec<u8>, StoreError>,
) -> Result<Vec<u8>, StoreError> {
    paths::validate(root, target, missing_ok)?;
    let outcome = root.atomic_mutate(
        target,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok,
                max_bytes: MAX_INPUT_BYTES,
            },
            create_parent: missing_ok,
            max_output: MAX_INDEX_BYTES,
            ..MutationOptions::default()
        },
        |old| {
            cancelled(interrupted)?;
            paths::validate(root, target, false)?;
            if expected.is_some_and(|expected| expected != old) {
                return Err(StoreError::Changed);
            }
            let new = transform(old)?;
            lines(&new)?;
            cancelled(interrupted)?;
            Ok(new)
        },
    )?;
    Ok(outcome.bytes)
}

/// Atomically append a validated batch, retaining every newly appended event.
/// Empty batches only read; they neither create files nor compact the index.
pub fn append_rows(
    repo: &Path,
    index: Option<&Path>,
    rows: &[&[u8]],
    options: &AppendOptions,
    interrupted: &AtomicBool,
) -> Result<Vec<u8>, StoreError> {
    cancelled(interrupted)?;
    let (keep, high) = retention_limits(options.keep, options.high_water)?;
    let mut required = BTreeSet::new();
    let mut bytes = 0usize;
    for line in rows {
        let parsed = row(line)?;
        let event = parsed["event_id"]
            .text()
            .filter(|s| !s.is_empty())
            .ok_or(StoreError::Invalid("appended row requires event_id"))?;
        if !required.insert(event.to_string()) {
            return Err(StoreError::Invalid("duplicate appended event_id"));
        }
        bytes = bytes
            .checked_add(line.len())
            .ok_or(StoreError::Invalid("append exceeds byte limit"))?;
        if bytes > MAX_INPUT_BYTES {
            return Err(StoreError::Invalid("append exceeds byte limit"));
        }
    }
    if rows.is_empty() {
        return read_index(repo, index, true);
    }
    let root = Root::open(repo)?;
    let target = canonical_index(repo, index, false, false)?;
    let _guard = acquire(&target, interrupted)?;
    append_locked(
        &root,
        &target,
        rows,
        keep,
        high,
        options.expected.as_deref(),
        &required.into_iter().collect::<Vec<_>>(),
        interrupted,
    )
}
#[allow(clippy::too_many_arguments)]
fn append_locked(
    root: &Root,
    target: &Path,
    rows: &[&[u8]],
    keep: usize,
    high: usize,
    expected: Option<&[u8]>,
    required: &[String],
    interrupted: &AtomicBool,
) -> Result<Vec<u8>, StoreError> {
    mutate(root, target, true, expected, interrupted, |old| {
        let mut candidate = lines(old)?;
        for line in &candidate {
            let parsed = row(line)?;
            if parsed["event_id"]
                .text()
                .is_some_and(|id| required.iter().any(|s| s == id))
            {
                return Err(StoreError::Invalid("duplicate artifact event_id"));
            }
        }
        candidate.extend_from_slice(rows);
        let size: usize = candidate.iter().map(|line| line.len()).sum();
        if candidate.len() > high || size > MAX_INDEX_BYTES {
            bounded(root.physical_root(), &candidate, keep, required)
        } else {
            Ok(candidate.concat())
        }
    })
}
fn bounded(
    repo: &Path,
    source: &[&[u8]],
    keep: usize,
    required: &[String],
) -> Result<Vec<u8>, StoreError> {
    let options = RetentionOptions {
        keep,
        required_event_ids: required.to_vec(),
        pinned_event_ids: completion_pins(repo),
        ..RetentionOptions::default()
    };
    Ok(retained_lines(source, &options)?.concat())
}

/// Replace a validated snapshot under the same lock and retention policy.
pub fn replace_bytes(
    repo: &Path,
    index: Option<&Path>,
    body: &[u8],
    options: &AppendOptions,
    required_event_ids: &[String],
    interrupted: &AtomicBool,
) -> Result<Vec<u8>, StoreError> {
    let (keep, _) = retention_limits(options.keep, options.keep)?;
    let source = lines(body)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, index, false, false)?;
    let _guard = acquire(&target, interrupted)?;
    let bounded = bounded(root.physical_root(), &source, keep, required_event_ids)?;
    mutate(
        &root,
        &target,
        false,
        options.expected.as_deref(),
        interrupted,
        |old| {
            lines(old)?;
            Ok(bounded)
        },
    )
}
