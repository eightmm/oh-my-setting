use crate::Result;
use crate::events::sort_list_attempts;
use crate::projection::Projection;
use crate::storage::{self, EventsReadSnapshot, LedgerPrefixMeta};
use crate::value::V;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncDisposition {
    Empty,
    Unchanged,
    Incremental { appended_rows: usize },
    Cold,
}

pub(crate) struct CachedRepo {
    canonical_repo: PathBuf,
    meta: LedgerPrefixMeta,
    phys_lines: usize,
    projection: Projection,
}

/// Read-only in-memory incremental cache for a single repository's lifecycle projection.
#[derive(Default)]
pub struct EventsProjectionCache {
    entry: Mutex<Option<CachedRepo>>,
}

impl EventsProjectionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self, repo: &Path, interrupted: &AtomicBool) -> Result<Vec<V>> {
        self.sync_with(repo, interrupted, Ok).map(|(v, _)| v)
    }

    pub fn list_json(&self, repo: &Path, interrupted: &AtomicBool) -> Result<Vec<u8>> {
        self.sync_with(repo, interrupted, |values| {
            let mut out = Vec::new();
            crate::print_json(&mut out, &V::Arr(values))?;
            Ok(out)
        })
        .map(|(out, _)| out)
    }

    #[cfg(test)]
    pub(crate) fn list_with_disposition(
        &self,
        repo: &Path,
        interrupted: &AtomicBool,
    ) -> Result<(SyncDisposition, Vec<V>)> {
        let (values, disposition) = self.sync_with(repo, interrupted, Ok)?;
        Ok((disposition, values))
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self) -> bool {
        self.entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub(crate) fn sync_with<T>(
        &self,
        repo: &Path,
        interrupted: &AtomicBool,
        render: impl FnOnce(Vec<V>) -> Result<T>,
    ) -> Result<(T, SyncDisposition)> {
        let prev = self.entry.lock().unwrap_or_else(|e| e.into_inner()).take();
        crate::checkpoint(interrupted)?;
        let canonical_repo = storage::resolve_read_repo(repo)?;
        let matched = prev.filter(|c| c.canonical_repo == canonical_repo);
        let snapshot = storage::read_events_snapshot(
            canonical_repo,
            matched.as_ref().map(|c| &c.meta),
            interrupted,
        )?;
        let (next, values, disposition) = apply_snapshot(snapshot, matched, interrupted)?;
        crate::checkpoint(interrupted)?;
        let output = render(values)?;
        crate::checkpoint(interrupted)?;
        *self.entry.lock().unwrap_or_else(|e| e.into_inner()) = next;
        Ok((output, disposition))
    }
}

pub(crate) fn apply_snapshot(
    snapshot: EventsReadSnapshot,
    prev: Option<CachedRepo>,
    interrupted: &AtomicBool,
) -> Result<(Option<CachedRepo>, Vec<V>, SyncDisposition)> {
    match (snapshot, prev) {
        (EventsReadSnapshot::Absent { canonical_repo }, _) => {
            drop(canonical_repo);
            crate::checkpoint(interrupted)?;
            Ok((None, Vec::new(), SyncDisposition::Empty))
        }
        (
            EventsReadSnapshot::Incremental {
                canonical_repo,
                path,
                meta,
                delta,
            },
            Some(mut entry),
        ) => {
            if entry.canonical_repo != canonical_repo {
                return Err("incremental ledger snapshot repository mismatch".into());
            }
            let disposition = if delta.is_empty() {
                SyncDisposition::Unchanged
            } else {
                let (delta_rows, delta_phys) =
                    storage::parse_rows_at(&path, &delta, entry.phys_lines, interrupted)?;
                entry.projection.extend_checked(&delta_rows, interrupted)?;
                entry.phys_lines += delta_phys;
                entry.meta = meta;
                SyncDisposition::Incremental {
                    appended_rows: delta_rows.len(),
                }
            };
            let mut values = entry.projection.ordered_attempts();
            sort_list_attempts(&mut values);
            crate::checkpoint(interrupted)?;
            Ok((Some(entry), values, disposition))
        }
        (EventsReadSnapshot::Incremental { .. }, None) => {
            Err("incremental ledger snapshot missing cached projection".into())
        }
        (
            EventsReadSnapshot::Full {
                canonical_repo,
                path,
                meta,
                bytes,
            },
            old,
        ) => {
            drop(old);
            let (rows, phys_lines) = storage::parse_rows_at(&path, &bytes, 0, interrupted)?;
            let mut projection = Projection::default();
            projection.extend_checked(&rows, interrupted)?;
            let mut values = projection.ordered_attempts();
            sort_list_attempts(&mut values);
            let entry = CachedRepo {
                canonical_repo,
                meta,
                phys_lines,
                projection,
            };
            crate::checkpoint(interrupted)?;
            Ok((Some(entry), values, SyncDisposition::Cold))
        }
    }
}
