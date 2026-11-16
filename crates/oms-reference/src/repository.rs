//! Shared state resolution for task, plan and room adapters. A room binding is
//! a consistency constraint; only a checked marker and Git registration bind
//! a parent's scratch checkout to its original shared repository.
use crate::{Result, checkpoint, storage};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};

pub const MARKER_LIMIT: usize = 16 * 1024;
pub const EXECUTION_ENV: &str = "OMS_PANEL_EXECUTION_ROOT";

#[derive(Clone, Debug)]
pub struct Resolver {
    pub managed_root: PathBuf,
    pub child: bool,
    pub inherited_room: Option<PathBuf>,
    pub execution_root: Option<PathBuf>,
}
impl Resolver {
    /// Snapshot ambient routing once; inherited values grant no ownership.
    pub fn from_env() -> Result<Self> {
        let get = |name| std::env::var_os(name).filter(|v| !v.is_empty());
        let managed_root = if let Some(root) = get("OMS_DELEGATE_WORKTREE_ROOT") {
            PathBuf::from(root)
        } else {
            let cache = get("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| get("HOME").map(|v| PathBuf::from(v).join(".cache")))
                .ok_or("managed state root unavailable")?;
            cache.join("oh-my-setting/worktrees")
        };
        let inherited_room = if get("OMS_ROOM_ID").is_some() {
            get("OMS_ROOM_REPO").map(PathBuf::from)
        } else {
            None
        };
        Ok(Self {
            managed_root,
            child: ["OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH"]
                .iter()
                .any(|name| get(name).is_some_and(|v| v != OsStr::new("0"))),
            inherited_room,
            execution_root: get(EXECUTION_ENV).map(PathBuf::from),
        })
    }
    pub fn state_repository(
        &self,
        input: &Path,
        allow_foreign: bool,
        interrupted: &AtomicBool,
    ) -> Result<PathBuf> {
        let shared = self.resolve(input, interrupted)?;
        if !allow_foreign
            && let Some(inherited) = &self.inherited_room
            && self.resolve(inherited, interrupted)? != shared
        {
            return Err("selected repository conflicts with the inherited room repository".into());
        }
        Ok(shared)
    }
    pub fn native_repository(&self, shared: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
        let Some(intended) = &self.execution_root else {
            return Ok(shared.to_owned());
        };
        let (working, _) = work_repository(intended, interrupted)?;
        if self.resolve(&working, interrupted)? != shared {
            return Err(
                "native execution root does not belong to the selected state repository".into(),
            );
        }
        Ok(working)
    }
    fn resolve(&self, input: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
        let (local, is_git) = work_repository(input, interrupted)?;
        if !is_git || self.child {
            return Ok(local);
        }
        let Some(parent) = harness_residue_parent(&local) else {
            return Ok(local);
        };
        if !parent
            .file_name()
            .is_some_and(|v| v.as_bytes().starts_with(b"oh-my-setting-scratch."))
        {
            return Ok(local);
        }
        let marker_path = parent.join(".oh-my-setting-tmp");
        let (fields, generation) = match marker(&marker_path) {
            Ok(v) => v,
            Err(_) => return Ok(local),
        };
        // A malformed or unregistered marker follows the existing local-root
        // fallback; inherited room/execution bindings are checked separately.
        let registration = (|| {
            let managed = physical_directory(&self.managed_root)?;
            if parent.parent() != Some(managed.as_path()) || physical_directory(&parent)? != parent
            {
                return Err("foreign scratch root".to_string());
            }
            let origin =
                physical_directory(Path::new(fields.get("repo").ok_or("missing marker repo")?))?;
            let intended = physical_directory(Path::new(
                fields.get("worktree").ok_or("missing marker worktree")?,
            ))?;
            if intended != local {
                return Err("marker worktree differs".into());
            }
            let common = git_path(&origin, "--git-common-dir", interrupted)?;
            if git_path(&local, "--git-common-dir", interrupted)? != common {
                return Err("foreign scratch common directory".into());
            }
            let git_dir = git_path(&local, "--git-dir", interrupted)?;
            let suffix = git_dir
                .strip_prefix(common.join("worktrees"))
                .map_err(|_| "scratch is not a linked worktree")?;
            if suffix.as_os_str().is_empty() {
                return Err("scratch registration is unavailable".into());
            }
            let listing = git(&origin, &["worktree", "list", "--porcelain"], interrupted)?;
            let worktrees = listing
                .split(|b| *b == b'\n')
                .filter_map(|v| v.strip_prefix(b"worktree "))
                .map(|v| PathBuf::from(OsStr::from_bytes(v)))
                .collect::<Vec<_>>();
            if !worktrees
                .iter()
                .any(|path| physical_directory(path).is_ok_and(|path| path == local))
            {
                return Err("scratch worktree is not registered".into());
            }
            let main =
                physical_directory(worktrees.first().ok_or("main worktree is unavailable")?)?;
            if main == local
                || git(&main, &["rev-parse", "--is-bare-repository"], interrupted)? != b"false\n"
                || git_path(&main, "--git-dir", interrupted)? != common
            {
                return Err("shared repository does not own the common directory".into());
            }
            Ok(main)
        })();
        checkpoint(interrupted)?;
        let main = match registration {
            Ok(main) => main,
            Err(_) => return Ok(local),
        };
        let Ok((after, info)) = marker(&marker_path) else {
            return Ok(local);
        };
        if fields != after || !storage::generation(&generation, &info) {
            return Ok(local);
        }
        Ok(main)
    }
}

pub fn harness_residue_parent(worktree: &Path) -> Option<PathBuf> {
    let parent = worktree.parent()?;
    let name = parent.file_name()?.to_str()?;
    let suffix = name.rsplit_once('.')?.1;
    if suffix.is_empty() {
        return None;
    }
    for (prefix, child) in [
        ("oh-my-setting-delegate", "wt"),
        ("oh-my-setting-admit", "wt"),
        ("oh-my-setting-scratch", "wt"),
        ("oh-my-setting-land", "wt"),
        ("oh-my-setting-agy-read", "tree"),
    ] {
        if name.starts_with(&format!("{prefix}."))
            && worktree.file_name().is_some_and(|v| {
                v == OsStr::new(child) || v == OsStr::new(&format!("{child}-{suffix}"))
            })
        {
            return Some(parent.to_owned());
        }
    }
    None
}
fn marker(path: &Path) -> Result<(BTreeMap<String, String>, fs::Metadata)> {
    let (raw, info) = storage::regular(path, MARKER_LIMIT, 1)?;
    if !raw.ends_with(b"\n") {
        return Err("scratch marker changed or is incomplete".into());
    }
    let mut fields = BTreeMap::new();
    for line in std::str::from_utf8(&raw)
        .map_err(|_| "invalid scratch marker text")?
        .lines()
    {
        let (key, value) = line
            .split_once('=')
            .ok_or("invalid scratch marker fields")?;
        if value.contains('\0') || fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err("invalid scratch marker fields".into());
        }
    }
    if fields.get("kind").map(String::as_str) != Some("oh-my-setting-temp")
        || fields.get("temporary").map(String::as_str) != Some("1")
        || ["repo", "worktree"]
            .iter()
            .any(|k| fields.get(*k).is_none_or(String::is_empty))
    {
        return Err("invalid scratch marker type or paths".into());
    }
    Ok((fields, info))
}
fn physical_directory(path: &Path) -> Result<PathBuf> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("repository must be a real directory".into());
    }
    fs::canonicalize(path).map_err(|e| e.to_string())
}
pub fn work_repository(input: &Path, interrupted: &AtomicBool) -> Result<(PathBuf, bool)> {
    let directory = fs::canonicalize(input).map_err(|e| e.to_string())?;
    if !directory.is_dir() {
        return Err("repository must be a directory".into());
    }
    match git(&directory, &["rev-parse", "--show-toplevel"], interrupted) {
        Ok(raw) => Ok((physical_directory(&query_path(&raw)?)?, true)),
        Err(_) => {
            checkpoint(interrupted)?;
            Ok((directory, false))
        }
    }
}
fn query_path(raw: &[u8]) -> Result<PathBuf> {
    let mut raw = raw.to_vec();
    while raw.last() == Some(&b'\n') {
        raw.pop();
    }
    if raw.is_empty() || raw.contains(&0) {
        return Err("invalid Git repository path".into());
    }
    Ok(PathBuf::from(OsString::from(OsStr::from_bytes(&raw))))
}
fn git_path(repo: &Path, query: &str, interrupted: &AtomicBool) -> Result<PathBuf> {
    let value = query_path(&git(repo, &["rev-parse", query], interrupted)?)?;
    physical_directory(&if value.is_absolute() {
        value
    } else {
        repo.join(value)
    })
}
pub(crate) fn git(repo: &Path, arguments: &[&str], interrupted: &AtomicBool) -> Result<Vec<u8>> {
    checkpoint(interrupted)?;
    let mut command = Command::new("git");
    command
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(repo)
        .args(arguments)
        .stdin(Stdio::null());
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    let outcome = command.output().map_err(|e| e.to_string())?;
    checkpoint(interrupted)?;
    if !outcome.status.success() {
        return Err("room repository resolution failed".into());
    }
    Ok(outcome.stdout)
}

/// FILE-only packets have a namespace root even when nested in a different
/// Git checkout. This is distinct from ordinary state routing.
pub fn reference_repository(
    target: &Path,
    resolver: &Resolver,
    interrupted: &AtomicBool,
) -> Result<PathBuf> {
    let absolute = storage::absolute(target)?;
    let parent = absolute.parent().ok_or("task file has no parent")?;
    let physical = fs::canonicalize(parent)
        .map_err(|_| "task file parent does not exist; supply its repository")?;
    for scope in physical.ancestors() {
        if let Some(owner) = scope.parent()
            && owner.file_name() == Some(OsStr::new(".oms"))
            && ["task", "tasks", "plan", "artifacts"]
                .iter()
                .any(|v| scope.file_name() == Some(OsStr::new(v)))
        {
            let root = owner
                .parent()
                .ok_or("task packet repository does not exist")?;
            let (git_root, is_git) = work_repository(root, interrupted)?;
            if !is_git || git_root != root {
                return Ok(root.to_owned());
            }
            return resolver.state_repository(root, false, interrupted);
        }
    }
    resolver.state_repository(&physical, false, interrupted)
}
