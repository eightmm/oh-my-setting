//! Native counterpart of room_repository's registered parent scratch resolver.
use crate::{MAX_ATTACHMENT, Result, UNSAFE, git, read};
use oms_safe_fs::Root;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub fn state(work: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
    // A delegated worker never maps its scratch checkout onto shared state.
    if crate::child() {
        return Ok(work.to_owned());
    }
    let Some(parent) = work.parent() else {
        return Ok(work.to_owned());
    };
    let Some(name) = parent.file_name().and_then(OsStr::to_str) else {
        return Ok(work.to_owned());
    };
    if !name.starts_with("oh-my-setting-scratch.") {
        return Ok(work.to_owned());
    }
    let suffix = name.rsplit('.').next().unwrap_or("");
    let leaf = work.file_name().unwrap_or_default();
    if suffix.is_empty() || leaf != "wt" && leaf != OsStr::new(&format!("wt-{suffix}")) {
        return Ok(work.to_owned());
    }
    // Invalid/unrecognized residue is local, exactly as the shared resolver.
    let admitted = (|| {
        let root = Root::open(parent).map_err(|_| UNSAFE)?;
        let marker = Path::new(".oh-my-setting-tmp");
        let generation = std::fs::symlink_metadata(parent.join(marker)).map_err(|_| UNSAFE)?;
        let bytes = read(&root, marker, 16 * 1024)?;
        if !bytes.ends_with(b"\n") {
            return Err(UNSAFE);
        }
        let mut fields = BTreeMap::new();
        for line in std::str::from_utf8(&bytes).map_err(|_| UNSAFE)?.lines() {
            let (key, value) = line.split_once('=').ok_or(UNSAFE)?;
            if value.contains('\0') || fields.insert(key, value).is_some() {
                return Err(UNSAFE);
            }
        }
        if fields.get("kind") != Some(&"oh-my-setting-temp")
            || fields.get("temporary") != Some(&"1")
        {
            return Err(UNSAFE);
        }
        let intended = Path::new(
            fields
                .get("worktree")
                .filter(|v| !v.is_empty())
                .ok_or(UNSAFE)?,
        )
        .canonicalize()
        .map_err(|_| UNSAFE)?;
        let origin = Path::new(fields.get("repo").filter(|v| !v.is_empty()).ok_or(UNSAFE)?)
            .canonicalize()
            .map_err(|_| UNSAFE)?;
        let managed = if let Some(path) =
            std::env::var_os("OMS_DELEGATE_WORKTREE_ROOT").filter(|v| !v.is_empty())
        {
            PathBuf::from(path)
        } else {
            let cache =
                if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
                    PathBuf::from(path)
                } else {
                    PathBuf::from(std::env::var_os("HOME").ok_or(UNSAFE)?).join(".cache")
                };
            cache.join("oh-my-setting/worktrees")
        }
        .canonicalize()
        .map_err(|_| UNSAFE)?;
        if parent.parent() != Some(&managed) || intended != work {
            return Err(UNSAFE);
        }
        let common = git_path(work, "--git-common-dir", interrupted)?;
        if git_path(&origin, "--git-common-dir", interrupted)? != common
            || !git_path(work, "--git-dir", interrupted)?.starts_with(common.join("worktrees"))
        {
            return Err(UNSAFE);
        }
        let listing = git(
            &origin,
            &[
                OsStr::new("worktree"),
                OsStr::new("list"),
                OsStr::new("--porcelain"),
            ],
            MAX_ATTACHMENT * 16,
            interrupted,
        )?;
        let registered = listing
            .split(|b| *b == b'\n')
            .filter_map(|line| line.strip_prefix(b"worktree "))
            .map(|bytes| PathBuf::from(OsString::from_vec(bytes.to_vec())))
            .collect::<Vec<_>>();
        if !registered
            .iter()
            .any(|p| p.canonicalize().is_ok_and(|p| p == work))
        {
            return Err(UNSAFE);
        }
        let main = registered
            .first()
            .ok_or(UNSAFE)?
            .canonicalize()
            .map_err(|_| UNSAFE)?;
        if main == work
            || git_path(&main, "--git-dir", interrupted)? != common
            || git(
                &main,
                &[OsStr::new("rev-parse"), OsStr::new("--is-bare-repository")],
                16,
                interrupted,
            )? != b"false\n"
        {
            return Err(UNSAFE);
        }
        let after = std::fs::symlink_metadata(parent.join(marker)).map_err(|_| UNSAFE)?;
        let shape = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.mode(),
                m.nlink(),
                m.size(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if shape(&generation) != shape(&after) || read(&root, marker, 16 * 1024)? != bytes {
            return Err(UNSAFE);
        }
        Ok(main)
    })();
    Ok(admitted.unwrap_or_else(|_| work.to_owned()))
}
fn git_path(work: &Path, option: &str, interrupted: &AtomicBool) -> Result<PathBuf> {
    let raw = git(
        work,
        &[OsStr::new("rev-parse"), OsStr::new(option)],
        MAX_ATTACHMENT,
        interrupted,
    )?;
    let raw = raw.strip_suffix(b"\n").unwrap_or(&raw);
    let path = PathBuf::from(OsString::from_vec(raw.to_vec()));
    if path.is_absolute() {
        path
    } else {
        work.join(path)
    }
    .canonicalize()
    .map_err(|_| UNSAFE)
}
