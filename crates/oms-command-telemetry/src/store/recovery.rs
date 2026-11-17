use super::*;
use crate::value::{encode, obj};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvageReport {
    pub healthy: bool,
    pub applied: bool,
    pub digest: Option<String>,
    pub quarantine: Option<String>,
    pub raw_bytes: usize,
    pub source_rows: usize,
    pub recovered_rows: usize,
    pub dropped_rows: usize,
    pub compacted_rows: usize,
    pub stored_rows: usize,
}
/// Quarantine the exact corrupt snapshot before replacing it with recovered rows
/// and a required receipt. A kill between publications leaves a reusable quarantine
/// and the original index; a published index always has its quarantine first.
pub fn salvage_index(
    repo: &Path,
    index: Option<&Path>,
    apply: bool,
    interrupted: &AtomicBool,
) -> Result<SalvageReport, StoreError> {
    cancelled(interrupted)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, index, false, false)?;
    let _guard = acquire(&target, interrupted)?;
    let raw = root.read_no_follow(
        &target,
        ByteReadOptions {
            missing_ok: false,
            max_bytes: MAX_INPUT_BYTES,
        },
    )?;
    let mut recovered = Vec::new();
    let mut source_rows = 0;
    let mut dropped = 0;
    for payload in raw.split_inclusive(|b| *b == b'\n') {
        source_rows += 1;
        let mut candidate = payload.to_vec();
        if !candidate.ends_with(b"\n") {
            candidate.push(b'\n');
        }
        if row(&candidate).is_ok() {
            recovered.push(candidate);
        } else {
            dropped += 1;
        }
    }
    let mut report = SalvageReport {
        healthy: false,
        applied: false,
        digest: None,
        quarantine: None,
        raw_bytes: raw.len(),
        source_rows,
        recovered_rows: recovered.len(),
        dropped_rows: dropped,
        compacted_rows: 0,
        stored_rows: recovered.len(),
    };
    if dropped == 0 && recovered.concat() == raw {
        report.healthy = true;
        return Ok(report);
    }
    let digest = format!("{:x}", Sha256::digest(&raw));
    let quarantine = format!(".oms/artifacts/quarantine/artifact-index-{digest}.raw");
    let mut existing = BTreeSet::new();
    for line in &recovered {
        if let Some(id) = row(line)?["event_id"].bytes() {
            existing.insert(id.to_vec());
        }
    }
    let event = loop {
        let event = format!("evt_salvage_{}", producer::random_hex::<16>()?);
        if !existing.contains(event.as_bytes()) {
            break event;
        }
    };
    let (keep, high) = retention_limits(None, None)?;
    let mut receipt = obj!("schema":1,"event_id":event.clone(),"operation_id":format!("op_salvage_{}",&digest[..32]),"artifact_id":format!("sha256:{digest}"),"ts":producer::now_text()?,"kind":"artifact-index-salvage","provider":"local","exit":0,"artifact":quarantine.clone(),"quarantine":quarantine.clone(),"quarantine_sha256":digest.clone(),"raw_bytes":raw.len(),"source_rows":source_rows,"recovered_rows":recovered.len(),"dropped_rows":dropped,"compacted_rows":0);
    let mut repaired = None;
    let mut compacted = 0;
    for _ in 0..16 {
        receipt["compacted_rows"] = compacted.into();
        let encoded =
            encode(&receipt).map_err(|_| StoreError::Invalid("cannot encode salvage receipt"))?;
        let mut candidate: Vec<&[u8]> = recovered.iter().map(Vec::as_slice).collect();
        candidate.push(&encoded);
        let size: usize = candidate.iter().map(|s| s.len()).sum();
        let body = if candidate.len() > high || size > MAX_INDEX_BYTES {
            bounded(
                root.physical_root(),
                &candidate,
                keep,
                std::slice::from_ref(&event),
            )?
        } else {
            candidate.concat()
        };
        let next = recovered.len() - (lines(&body)?.len() - 1);
        if next == compacted {
            repaired = Some(body);
            break;
        }
        compacted = next;
    }
    let repaired = repaired.ok_or(StoreError::Invalid(
        "salvage retention counts did not stabilize",
    ))?;
    report.digest = Some(digest);
    report.quarantine = Some(quarantine.clone());
    report.compacted_rows = compacted;
    report.stored_rows = lines(&repaired)?.len();
    if !apply {
        return Ok(report);
    }
    cancelled(interrupted)?;
    let qpath = root.physical_root().join(&quarantine);
    paths::validate(&root, &qpath, true)?;
    let qparent = paths::Directory::open(
        &root,
        qpath
            .parent()
            .ok_or(StoreError::Invalid("quarantine parent"))?,
        false,
    )?;
    let name = qpath
        .file_name()
        .ok_or(StoreError::Invalid("quarantine name"))?;
    match qparent.stat(name) {
        Ok(_) => verify_quarantine(&root, &qpath, &raw)?,
        Err(error) if paths::missing(&error) => {
            let result = root.atomic_mutate(
                &qpath,
                MutationOptions {
                    read: ByteReadOptions {
                        missing_ok: true,
                        max_bytes: MAX_INPUT_BYTES,
                    },
                    create_parent: true,
                    max_output: MAX_INPUT_BYTES,
                    expected_exists: Some(false),
                    ..MutationOptions::default()
                },
                |_| Ok::<_, StoreError>(raw.clone()),
            );
            match result {
                Ok(_) => (),
                Err(MutationError::Storage {
                    phase: MutationPhase::BeforePublication,
                    error: ByteError::ExistenceMismatch,
                }) => (),
                Err(error) => return Err(error.into()),
            }
            verify_quarantine(&root, &qpath, &raw)?;
        }
        Err(error) => return Err(error),
    }
    mutate(&root, &target, false, Some(&raw), interrupted, |_| {
        Ok(repaired.clone())
    })?;
    let verified = root.read_no_follow(
        &target,
        ByteReadOptions {
            missing_ok: false,
            max_bytes: MAX_INDEX_BYTES,
        },
    )?;
    if verified != repaired {
        return Err(StoreError::Changed);
    }
    verify_quarantine(&root, &qpath, &raw)?;
    report.applied = true;
    Ok(report)
}
fn verify_quarantine(root: &Root, path: &Path, raw: &[u8]) -> Result<(), StoreError> {
    paths::validate(root, path, false)?;
    let stored = root.read_no_follow(
        path,
        ByteReadOptions {
            missing_ok: false,
            max_bytes: MAX_INPUT_BYTES,
        },
    )?;
    let parent = paths::Directory::open(
        root,
        path.parent()
            .ok_or(StoreError::Invalid("quarantine parent"))?,
        false,
    )?;
    let stat = parent.stat(
        path.file_name()
            .ok_or(StoreError::Invalid("quarantine name"))?,
    )?;
    if stored != raw || !paths::regular(&stat) || stat.st_mode & 0o7777 != 0o600 {
        return Err(StoreError::Invalid(
            "content-addressed quarantine collision or unsafe mode",
        ));
    }
    parent.verify()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanReport {
    pub paths: Vec<String>,
    pub fresh: usize,
}
struct Candidate {
    parent: std::sync::Arc<paths::Directory>,
    name: OsString,
    relative: String,
    before: rustix::fs::Stat,
}

/// Sweep only owned single-link bodies under .oms/artifacts, with a stable index
/// snapshot and completion namespace. Scan limits refuse before the first unlink.
/// Deletion is incremental: interruption can leave a safely deleted prefix.
pub fn delete_orphans(
    repo: &Path,
    index: Option<&Path>,
    kept_body: &[u8],
    dry_run: bool,
    grace: i64,
    interrupted: &AtomicBool,
) -> Result<OrphanReport, StoreError> {
    sweep_orphans(
        repo,
        index,
        kept_body,
        kept_body,
        dry_run,
        grace,
        interrupted,
    )
}
pub(super) fn plan_orphans(
    repo: &Path,
    index: Option<&Path>,
    expected: &[u8],
    kept: &[u8],
    grace: i64,
    interrupted: &AtomicBool,
) -> Result<OrphanReport, StoreError> {
    sweep_orphans(repo, index, expected, kept, true, grace, interrupted)
}
#[allow(clippy::too_many_arguments)]
fn sweep_orphans(
    repo: &Path,
    index: Option<&Path>,
    expected: &[u8],
    kept_body: &[u8],
    dry_run: bool,
    grace: i64,
    interrupted: &AtomicBool,
) -> Result<OrphanReport, StoreError> {
    if grace < 0 {
        return Err(StoreError::Invalid("orphan grace must be non-negative"));
    }
    cancelled(interrupted)?;
    let source = lines(kept_body)?;
    let root = Root::open(repo)?;
    let target = canonical_index(repo, index, false, false)?;
    let _guard = acquire(&target, interrupted)?;
    let current = root.read_no_follow(
        &target,
        ByteReadOptions {
            missing_ok: true,
            max_bytes: MAX_INPUT_BYTES,
        },
    )?;
    if current != expected {
        return Err(StoreError::Changed);
    }
    let Some(completion_refs) = completion_references(root.physical_root()) else {
        return Ok(OrphanReport::default());
    };
    let artifacts = root.physical_root().join(".oms/artifacts");
    let boundary = match paths::Directory::open(&root, &artifacts, false) {
        Err(error) if paths::missing(&error) => return Ok(OrphanReport::default()),
        other => other?,
    };
    let mut referenced = BTreeSet::new();
    let mut protect = |value: &str| {
        let portable = value.replace('\\', "/");
        if portable.as_bytes().get(1) == Some(&b':') {
            return;
        }
        let candidate = root.physical_root().join(portable);
        if let Ok(canonical) = std::fs::canonicalize(candidate)
            && canonical.starts_with(&artifacts)
        {
            referenced.insert(canonical);
        }
    };
    for path in &completion_refs.1 {
        protect(path);
    }
    for line in source {
        let row = row(line)?;
        for key in ["artifact", "patch", "source"] {
            if let Some(path) = row[key].text() {
                protect(path);
            }
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Io)?
        .as_secs() as i128;
    let mut pending = vec![(artifacts.clone(), 0usize)];
    let mut candidates = Vec::new();
    let mut report = OrphanReport::default();
    let mut count = 0usize;
    while let Some((path, depth)) = pending.pop() {
        cancelled(interrupted)?;
        if depth > 64 {
            return Err(StoreError::Invalid("orphan scan depth exceeded"));
        }
        let parent = std::sync::Arc::new(paths::Directory::open(&root, &path, false)?);
        let before = rustix::fs::fstat(&parent.fd)?;
        let names = parent.names(65536usize.saturating_sub(count))?;
        count += names.len();
        for name in names {
            let stat = parent.stat(&name)?;
            let name_text = name.to_str();
            if name_text.is_some_and(|s| s.ends_with(".lock")) {
                continue;
            }
            let child = path.join(&name);
            // Native dispatch owns reservation custody and its evidence. An
            // absent artifact-index row or old mtime cannot settle those claims.
            if depth == 0 && name_text == Some("panel-dispatch") {
                continue;
            }
            if rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory
            {
                if depth == 0 && name_text == Some("output-digest") {
                    continue;
                }
                pending.push((child, depth + 1));
                continue;
            }
            if !paths::regular(&stat)
                || matches!(name_text, Some("index.jsonl" | ".gitignore"))
                || child == target
                || referenced.contains(&child)
            {
                continue;
            }
            if now - (stat.st_mtime as i128) < grace as i128 {
                report.fresh += 1;
                continue;
            }
            let relative = child
                .strip_prefix(root.physical_root())
                .map_err(|_| StoreError::Changed)?
                .to_str()
                .ok_or(StoreError::Invalid("orphan path is not UTF-8"))?
                .to_string();
            candidates.push(Candidate {
                parent: std::sync::Arc::clone(&parent),
                name,
                relative,
                before: stat,
            });
        }
        parent.verify()?;
        if !paths::unchanged(&before, &rustix::fs::fstat(&parent.fd)?) {
            return Err(StoreError::Changed);
        }
    }
    boundary.verify()?;
    // A receipt published during the scan cannot authorize deleting its payload.
    if completion_references(root.physical_root()).as_ref() != Some(&completion_refs) {
        return Err(StoreError::Changed);
    }
    candidates.sort_by(|a, b| a.relative.cmp(&b.relative));
    for candidate in candidates {
        cancelled(interrupted)?;
        boundary.verify()?;
        candidate.parent.verify()?;
        let after = candidate.parent.stat(&candidate.name)?;
        if !paths::regular(&after) || !paths::unchanged(&candidate.before, &after) {
            return Err(StoreError::Changed);
        }
        if !dry_run {
            rustix::fs::unlinkat(
                &candidate.parent.fd,
                &candidate.name,
                rustix::fs::AtFlags::empty(),
            )?;
            paths::sync(&candidate.parent.fd).map_err(|error| match error {
                StoreError::Storage { error, .. } => StoreError::Storage {
                    phase: MutationPhase::Published,
                    error,
                },
                other => other,
            })?;
        }
        report.paths.push(candidate.relative);
    }
    Ok(report)
}
