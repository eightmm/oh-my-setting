//! Reversible history transfers under the active allocator lock.
use crate::terminals::{MAX_ARCHIVED, MAX_ITEMS, MAX_RECORD, PrivateStore};
use crate::{Error, Result, Value, require};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Permanently remove exactly one bound record after a positive absence proof.
/// The allocator excludes allocation/transfer/update while the record lock also
/// fences legacy supervisors. Unexpected or unsafe entries preserve all data.
pub fn forget_record(
    repo: &Path,
    kind: &str,
    ident: &str,
    proof: impl FnOnce(&Value) -> Result<bool>,
    files: &[&str],
) -> Result<Value> {
    crate::parent_only()?;
    require(
        crate::terminals::valid_id(ident),
        "invalid managed record ID",
    )?;
    let expected: &[&str] = if kind == "jobs" {
        &["record", "recipe", "output"]
    } else {
        &["record"]
    };
    require(files == expected, "invalid private record file contract")?;
    let active = PrivateStore::open(repo, kind, false, true)?;
    let _allocator = active
        .store
        .lock_at(Path::new("lock"), Duration::from_secs(3))?;
    let record = PathBuf::from(ident).join("record");
    let source = if active.store.json(&record, MAX_RECORD, true)?.is_some() {
        PrivateStore::open(repo, kind, false, false)?
    } else {
        PrivateStore::open(repo, kind, true, false)?
    };
    source.record(ident)?;
    let _record = source
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let row = source.record(ident)?;
    let expected_kind = match kind {
        "jobs" => "job",
        "terminals" => "shell",
        "workspaces" => "workspace",
        _ => return Err(Error::new("invalid managed record kind")),
    };
    require(row["kind"] == expected_kind, "invalid private record kind")?;
    require(
        proof(&row)?,
        "record disappearance or completion is not proven",
    )?;
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = fs::open(&source.root, flags, Mode::empty()).map_err(Error::new)?;
    let directory = fs::openat(&parent, ident, flags, Mode::empty()).map_err(Error::new)?;
    let names = std::fs::read_dir(source.root.join(ident))
        .map_err(Error::new)?
        .map(|v| v.map(|v| v.file_name()).map_err(Error::new))
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    let expected = files
        .iter()
        .copied()
        .chain(["lock"])
        .map(std::ffi::OsString::from)
        .collect::<std::collections::BTreeSet<_>>();
    require(names == expected, "unexpected private record contents")?;
    for name in &names {
        let fd = fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(Error::new)?;
        let info = fs::fstat(&fd).map_err(Error::new)?;
        require(
            FileType::from_raw_mode(info.st_mode) == FileType::RegularFile
                && info.st_nlink == 1
                && info.st_uid == rustix::process::getuid().as_raw()
                && info.st_mode & 0o077 == 0,
            "unsafe private record file",
        )?;
    }
    let outcome = (|| -> Result<()> {
        for name in files.iter().copied().chain(["lock"]) {
            fs::unlinkat(&directory, name, AtFlags::empty()).map_err(Error::new)?;
        }
        fs::fsync(&directory).map_err(Error::new)?;
        fs::unlinkat(&parent, ident, AtFlags::REMOVEDIR).map_err(Error::new)?;
        fs::fsync(&parent).map_err(Error::new)
    })();
    outcome.map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    Ok(serde_json::json!({"id":ident,"kind":kind,"forgotten":true,
        "notice":"Private history and any retained log were permanently deleted."}))
}
pub fn capacity(repo: &Path, kind: &str) -> Result<Value> {
    crate::parent_only()?;
    let active = PrivateStore::open(repo, kind, false, true)?;
    let history = PrivateStore::open(repo, kind, true, true)?;
    Ok(
        serde_json::json!({"used":active.identifiers()?.len(),"limit":MAX_ITEMS,"archived":history.identifiers()?.len(),"archive_limit":MAX_ARCHIVED}),
    )
}
/// Proof must positively establish completion/absence from the owning native host.
pub fn transfer(
    repo: &Path,
    kind: &str,
    ident: &str,
    archived: bool,
    proof: impl FnOnce(&Value) -> Result<bool>,
) -> Result<Value> {
    crate::parent_only()?;
    require(
        crate::terminals::valid_id(ident),
        "invalid managed history ID",
    )?;
    let active = PrivateStore::open(repo, kind, false, true)?;
    let _allocator = active
        .store
        .lock_at(Path::new("lock"), Duration::from_secs(3))?;
    let source = PrivateStore::open(repo, kind, !archived, false)?;
    let destination = PrivateStore::open(repo, kind, archived, true)?;
    let source_path = PathBuf::from(ident).join("record");
    let _record = source
        .store
        .lock_at(&PathBuf::from(ident).join("lock"), Duration::from_secs(3))?;
    let row = source
        .store
        .json(&source_path, MAX_RECORD, false)?
        .ok_or_else(|| Error::new("history source unavailable"))?;
    require(
        row["id"] == ident
            && row["repo"] == source.repo.to_string_lossy().as_ref()
            && row["uid"].as_u64() == Some(u64::from(rustix::process::getuid().as_raw())),
        "history source binding changed",
    )?;
    require(
        proof(&row)?,
        "only positively completed or gone records can move to history",
    )?;
    require(
        destination.identifiers()?.len() < if archived { MAX_ARCHIVED } else { MAX_ITEMS },
        "history or active capacity is full; records preserved",
    )?;
    let files: std::collections::BTreeSet<_> = std::fs::read_dir(source.root.join(ident))
        .map_err(Error::new)?
        .map(|v| {
            v.map(|v| v.file_name().to_string_lossy().into_owned())
                .map_err(Error::new)
        })
        .collect::<Result<_>>()?;
    let allowed: std::collections::BTreeSet<_> = if kind == "jobs" {
        ["lock", "record", "recipe", "output"]
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        ["lock", "record"].into_iter().map(String::from).collect()
    };
    require(files == allowed, "unexpected private record contents")?;
    let mut changed = row.clone();
    if archived {
        changed["archived"] = true.into();
        changed["archived_at"] = crate::timestamp().into();
    } else {
        let map = changed
            .as_object_mut()
            .ok_or_else(|| Error::new("invalid history record"))?;
        map.remove("archived");
        map.remove("archived_at");
    }
    // Source may be active, whose allocator is already held above.
    let publish = if source.archived {
        Some(
            source
                .store
                .lock_at(Path::new("lock"), Duration::from_secs(3))?,
        )
    } else {
        None
    };
    let tx = publish.as_ref().unwrap_or(&_allocator);
    tx.write_json(&source_path, &changed, false, MAX_RECORD)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let from =
        rustix::fs::open(&source.root, flags, rustix::fs::Mode::empty()).map_err(Error::new)?;
    let to = rustix::fs::open(&destination.root, flags, rustix::fs::Mode::empty())
        .map_err(Error::new)?;
    require(
        !destination
            .root
            .join(ident)
            .try_exists()
            .map_err(Error::new)?,
        "history identity already exists; reconcile marker before retry",
    )?;
    rustix::fs::renameat(&from, ident, &to, ident).map_err(|e| Error {
        publication_uncertain: true,
        ..Error::new(format!(
            "history transfer outcome uncertain: {e}; reconcile either location"
        ))
    })?;
    rustix::fs::fsync(&from)
        .and_then(|()| rustix::fs::fsync(&to))
        .map_err(|e| Error {
            publication_uncertain: true,
            ..Error::new(e)
        })?;
    Ok(serde_json::json!({"id":ident,"archived":archived,"history_preserved":true}))
}
pub fn archive(
    repo: &Path,
    kind: &str,
    ident: &str,
    proof: impl FnOnce(&Value) -> Result<bool>,
) -> Result<Value> {
    transfer(repo, kind, ident, true, proof)
}
pub fn restore(
    repo: &Path,
    kind: &str,
    ident: &str,
    proof: impl FnOnce(&Value) -> Result<bool>,
) -> Result<Value> {
    transfer(repo, kind, ident, false, proof)
}
