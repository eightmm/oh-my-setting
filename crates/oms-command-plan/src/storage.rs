//! Bounded descriptor-relative state access, layered on the shared byte writer.
use crate::json::{self, Value, number, object};
use crate::{Result, error, require};
use oms_safe_fs::{ByteError, ByteReadOptions, MutationError, MutationOptions, Root};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One admission validation pass. Only the parent producer can fill the proof
/// set; all used groups are freshly revalidated before returning a binding.
pub struct GroupProofSnapshot {
    state: Mutex<(BTreeMap<u32, crate::services::GroupMembership>, Option<u32>)>,
}
impl GroupProofSnapshot {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new((BTreeMap::new(), None)),
        }
    }
    fn get(&self, gid: u32) -> Result<crate::services::GroupMembership> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("group proof snapshot unavailable"))?;
        if let Some(proof) = state.0.get(&gid) {
            return Ok(proof.clone());
        }
        state.1 = Some(gid);
        Err(error("bounded native group ownership proof required"))
    }
    pub(crate) fn missing(&self) -> Result<Option<u32>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("group proof snapshot unavailable"))?
            .1
            .take())
    }
    pub(crate) fn insert(&self, gid: u32, proof: crate::services::GroupMembership) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("group proof snapshot unavailable"))?;
        require(
            proof.gid == gid && !state.0.contains_key(&gid) && state.0.len() < 128,
            "invalid or excessive group ownership proofs",
        )?;
        state.0.insert(gid, proof);
        Ok(())
    }
    pub(crate) fn proofs(&self) -> Result<Vec<crate::services::GroupMembership>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("group proof snapshot unavailable"))?
            .0
            .values()
            .cloned()
            .collect())
    }
}

pub const MAX_JSON: usize = 4 * 1024 * 1024;
pub const MAX_STREAM: usize = 16 * 1024 * 1024;
pub const MAX_ROW: usize = 1024 * 1024;
pub const MAX_PATCH: usize = 16 * 1024 * 1024;
pub const ACTIVE: &str = ".oms/plan/tasks.json";

pub struct Store {
    root: Root,
    groups: Option<crate::services::GroupResolver>,
    group_snapshot: Option<Arc<GroupProofSnapshot>>,
}
pub struct FileSnapshot {
    pub raw: Vec<u8>,
    pub generation: Value,
    pub mode: u32,
}
fn io(e: impl std::fmt::Display) -> crate::Error {
    error(format!("plan storage: {e}"))
}
struct SnapshotReadError(crate::Error);

impl From<ByteError> for SnapshotReadError {
    fn from(e: ByteError) -> Self {
        Self(error(format!("plan read: {e:?}")))
    }
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn identity(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn stable(a: &Stat, b: &Stat) -> bool {
    identity(a, b)
        && a.st_size == b.st_size
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_gid == b.st_gid
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn trusted(
    info: &Stat,
    directory: bool,
    private: bool,
    groups: Option<crate::services::GroupResolver>,
    snapshot: Option<&GroupProofSnapshot>,
) -> Result<()> {
    let mode = info.st_mode & 0o7777;
    // Group-writable records need an account-database proof. This crate never
    // infers that proof from primary gid or effective group membership.
    let private_group = if mode & 0o020 == 0 {
        true
    } else {
        let membership = match snapshot {
            Some(snapshot) => snapshot.get(info.st_gid)?,
            None => groups
                .ok_or_else(|| error("native private-group account proof service unavailable"))?(
                info.st_gid,
            )?,
        };
        let uid = rustix::process::getuid().as_raw();
        membership.gid == info.st_gid
            && membership
                .explicit_member_uids
                .iter()
                .all(|member| *member == uid)
            && membership.primary_member_uids == [uid]
    };
    require(
        info.st_uid == rustix::process::getuid().as_raw()
            && mode & 0o002 == 0
            && private_group
            && if directory {
                FileType::from_raw_mode(info.st_mode) == FileType::Directory
                    && mode & 0o500 == 0o500
            } else {
                FileType::from_raw_mode(info.st_mode) == FileType::RegularFile
                    && info.st_nlink == 1
                    && mode & 0o400 != 0
            },
        "unsafe plan ownership, permissions or links",
    )?;
    require(
        !private || mode == if directory { 0o700 } else { 0o600 },
        "private plan object requires exact private mode",
    )
}
pub fn generation(info: &Stat) -> Value {
    object([
        ("dev", number(info.st_dev)),
        ("ino", number(info.st_ino)),
        ("size", number(info.st_size)),
        (
            "mtime_ns",
            number(i128::from(info.st_mtime) * 1_000_000_000 + i128::from(info.st_mtime_nsec)),
        ),
        (
            "ctime_ns",
            number(i128::from(info.st_ctime) * 1_000_000_000 + i128::from(info.st_ctime_nsec)),
        ),
    ])
}
pub fn valid_generation(v: &Value) -> bool {
    json::keys(v, &["dev", "ino", "size", "mtime_ns", "ctime_ns"], &[]).is_ok()
        && v.object()
            .is_ok_and(|a| a.iter().all(|(_, v)| v.int().is_ok_and(|n| n >= 0)))
}
struct Anchor {
    dirs: Vec<(OwnedFd, Stat, OsString)>,
    root: PathBuf,
    leaf: OsString,
    groups: Option<crate::services::GroupResolver>,
    group_snapshot: Option<Arc<GroupProofSnapshot>>,
}
impl Anchor {
    fn trusted(&self, info: &Stat, directory: bool, private: bool) -> Result<()> {
        trusted(
            info,
            directory,
            private,
            self.groups,
            self.group_snapshot.as_deref(),
        )
    }
    fn check(&self) -> Result<()> {
        for (index, (fd, initial, name)) in self.dirs.iter().enumerate() {
            let now = fs::fstat(fd).map_err(io)?;
            let named = if index == 0 {
                fs::statat(rustix::fs::CWD, &self.root, AtFlags::SYMLINK_NOFOLLOW)
            } else {
                fs::statat(&self.dirs[index - 1].0, name, AtFlags::SYMLINK_NOFOLLOW)
            }
            .map_err(io)?;
            self.trusted(&now, true, false)?;
            self.trusted(&named, true, false)?;
            require(
                identity(&now, initial) && identity(&now, &named),
                "plan directory changed during access",
            )?;
        }
        Ok(())
    }
    fn parent(&self) -> &OwnedFd {
        &self.dirs.last().expect("root held").0
    }
    fn stat(&self) -> Result<Option<Stat>> {
        self.check()?;
        match fs::statat(self.parent(), &self.leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => Ok(Some(s)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(io(e)),
        }
    }
}
impl Store {
    fn trusted(&self, info: &Stat, directory: bool, private: bool) -> Result<()> {
        trusted(
            info,
            directory,
            private,
            self.groups,
            self.group_snapshot.as_deref(),
        )
    }
    pub fn with_group_resolver(mut self, groups: Option<crate::services::GroupResolver>) -> Self {
        self.groups = groups;
        self.group_snapshot = None;
        self
    }
    pub(crate) fn with_group_snapshot(mut self, snapshot: Option<Arc<GroupProofSnapshot>>) -> Self {
        self.group_snapshot = snapshot;
        self
    }
    pub fn open(repo: &Path) -> Result<Self> {
        Ok(Self {
            root: Root::open(repo)
                .map_err(|e| error(format!("cannot open plan repository: {e:?}")))?,
            groups: None,
            group_snapshot: None,
        })
    }
    pub fn repo(&self) -> &Path {
        self.root.physical_root()
    }
    fn relative(&self, path: &Path) -> Result<PathBuf> {
        let rel = if path.is_absolute() {
            path.strip_prefix(self.repo())
                .map_err(|_| error("path is outside plan repository"))?
        } else {
            path
        };
        require(
            !rel.as_os_str().as_bytes().contains(&0)
                && rel
                    .components()
                    .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
            "invalid plan storage path",
        )?;
        require(rel.file_name().is_some(), "path must name a file")?;
        Ok(rel.to_path_buf())
    }
    fn anchor(&self, path: &Path) -> Result<Anchor> {
        let rel = self.relative(path)?;
        let fd = fs::open(self.repo(), flags(), Mode::empty()).map_err(io)?;
        let initial = fs::fstat(&fd).map_err(io)?;
        self.trusted(&initial, true, false)?;
        let mut a = Anchor {
            dirs: vec![(fd, initial, OsString::new())],
            root: self.repo().to_path_buf(),
            leaf: rel.file_name().unwrap().to_owned(),
            groups: self.groups,
            group_snapshot: self.group_snapshot.clone(),
        };
        for part in rel.parent().unwrap_or(Path::new("")).components() {
            if let Component::Normal(name) = part {
                let fd = fs::openat(a.parent(), name, flags(), Mode::empty()).map_err(io)?;
                let info = fs::fstat(&fd).map_err(io)?;
                self.trusted(&info, true, false)?;
                a.dirs.push((fd, info, name.to_owned()));
            }
        }
        a.check()?;
        Ok(a)
    }
    pub fn read(&self, path: impl AsRef<Path>, limit: usize) -> Result<Vec<u8>> {
        self.snapshot(path, limit, false)?
            .map(|s| s.raw)
            .ok_or_else(|| error("plan object is missing"))
    }
    pub fn optional(&self, path: impl AsRef<Path>, limit: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.snapshot(path, limit, true)?.map(|s| s.raw))
    }
    #[allow(clippy::unnecessary_cast)]
    pub fn snapshot(
        &self,
        path: impl AsRef<Path>,
        limit: usize,
        missing: bool,
    ) -> Result<Option<FileSnapshot>> {
        let path = path.as_ref();
        // The shared reader determines absence safely, including missing
        // ancestors. Never turn unsafe ancestry into an absent record.
        let a = match self.anchor(path) {
            Ok(a) => a,
            Err(e) if missing => {
                // Re-run the strict existence read; a permission/link failure must
                // retain its error even when the allowed missing body was empty.
                match self.root.read_no_follow(
                    path,
                    ByteReadOptions {
                        missing_ok: false,
                        max_bytes: limit,
                    },
                ) {
                    Err(oms_safe_fs::ByteError::Missing) => return Ok(None),
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        let Some(before) = a.stat()? else {
            return if missing {
                Ok(None)
            } else {
                Err(error("plan object is missing"))
            };
        };
        self.trusted(&before, false, false)?;
        let (raw, after) = self
            .root
            .read_no_follow_checked(
                path,
                ByteReadOptions {
                    missing_ok: false,
                    max_bytes: limit,
                },
                |opened| {
                    self.trusted(opened, false, false)
                        .and_then(|()| {
                            require(stable(&before, opened), "plan object changed during open")
                        })
                        .map_err(SnapshotReadError)
                },
            )
            .map_err(|e| e.0)?;
        let after = after.ok_or_else(|| error("plan object disappeared"))?;
        let named = a.stat()?.ok_or_else(|| error("plan object disappeared"))?;
        require(
            stable(&before, &after) && stable(&after, &named),
            "plan object changed during read",
        )?;
        Ok(Some(FileSnapshot {
            raw,
            generation: generation(&after),
            mode: after.st_mode as u32 & 0o7777,
        }))
    }
    pub fn value(&self, path: impl AsRef<Path>, limit: usize) -> Result<Value> {
        json::parse(&self.read(path, limit)?)
    }
    pub fn rows(&self, path: impl AsRef<Path>, missing: bool) -> Result<Vec<Value>> {
        let raw = if missing {
            self.optional(path, MAX_STREAM)?.unwrap_or_default()
        } else {
            self.read(path, MAX_STREAM)?
        };
        rows(&raw)
    }
    pub fn mutate(
        &self,
        path: impl AsRef<Path>,
        old: Option<&[u8]>,
        new: &[u8],
        limit: usize,
        create: bool,
    ) -> Result<()> {
        self.mutate_checked(path, old, new, limit, create, None::<fn() -> Result<()>>)
    }
    pub(crate) fn mutate_checked(
        &self,
        path: impl AsRef<Path>,
        old: Option<&[u8]>,
        new: &[u8],
        limit: usize,
        create: bool,
        check: Option<impl FnOnce() -> Result<()>>,
    ) -> Result<()> {
        let path = path.as_ref();
        self.relative(path)?;
        self.validate_write_parent(path, create)?;
        if let Ok(a) = self.anchor(path)
            && let Some(s) = a.stat()?
        {
            self.trusted(&s, false, false)?;
        }
        let options = MutationOptions {
            read: ByteReadOptions {
                missing_ok: create,
                max_bytes: limit,
            },
            create_parent: create,
            max_output: limit,
            expected_exists: if !create { Some(true) } else { None },
            ..Default::default()
        };
        let transform = |bytes: &[u8]| {
            if let Some(expected) = old {
                require(bytes == expected, "plan CAS drift before publication")?;
            }
            Ok::<_, crate::Error>(new.to_vec())
        };
        match check {
            Some(check) => self
                .root
                .atomic_mutate_checked(path, options, transform, check),
            None => self.root.atomic_mutate(path, options, transform),
        }
        .map_err(|e| match e {
            MutationError::Transform(e) => e,
            MutationError::Storage { phase, error: e } => error(format!(
                "plan write {phase:?}: {e:?}; reconcile before retry"
            )),
        })?;
        let a = self.anchor(path)?;
        let s = a
            .stat()?
            .ok_or_else(|| error("published plan object is missing"))?;
        self.trusted(&s, false, false)?;
        Ok(())
    }
    fn validate_write_parent(&self, path: &Path, create: bool) -> Result<()> {
        let relative = self.relative(path)?;
        let mut fd = fs::open(self.repo(), flags(), Mode::empty()).map_err(io)?;
        self.trusted(&fs::fstat(&fd).map_err(io)?, true, false)?;
        for part in relative.parent().unwrap_or(Path::new("")).components() {
            if let Component::Normal(name) = part {
                match fs::openat(&fd, name, flags(), Mode::empty()) {
                    Ok(child) => {
                        self.trusted(&fs::fstat(&child).map_err(io)?, true, false)?;
                        fd = child;
                    }
                    Err(rustix::io::Errno::NOENT) if create => return Ok(()),
                    Err(e) => return Err(io(e)),
                }
            }
        }
        Ok(())
    }
    pub fn immutable(
        &self,
        directory: &str,
        raw: &[u8],
        suffix: &str,
        limit: usize,
    ) -> Result<(Value, bool)> {
        require(raw.len() <= limit, "immutable plan object exceeds limit")?;
        let hash = json::sha(raw);
        let rel = format!("{directory}/{hash}{suffix}");
        let old = self.optional(&rel, limit)?;
        if let Some(ref bytes) = old {
            require(
                bytes == raw,
                "immutable plan object is occupied by different bytes",
            )?;
        }
        self.mutate(&rel, old.as_deref().or(Some(&[])), raw, limit, true)?;
        let a = self.anchor(Path::new(&rel))?;
        self.trusted(&fs::fstat(a.parent()).map_err(io)?, true, true)?;
        self.trusted(
            &a.stat()?
                .ok_or_else(|| error("immutable plan object is missing"))?,
            false,
            true,
        )?;
        self.sync(&rel)?;
        Ok((
            object([("path", json::string(&rel)), ("sha256", json::string(hash))]),
            old.is_none(),
        ))
    }
    pub fn append_private_atomic(&self, path: impl AsRef<Path>, row: &Value) -> Result<()> {
        let path = path.as_ref();
        let snapshot = self.snapshot(path, MAX_STREAM, true)?;
        if let Some(snapshot) = &snapshot {
            require(
                snapshot.mode == 0o600,
                "private plan object requires exact private mode",
            )?;
        }
        let old = snapshot
            .as_ref()
            .map(|s| s.raw.as_slice())
            .unwrap_or_default();
        rows(old)?;
        let encoded = json::encoded(row)?;
        require(
            encoded.len() <= MAX_ROW && old.len() + encoded.len() <= MAX_STREAM,
            "plan ledger exceeds bound",
        )?;
        let mut new = old.to_vec();
        new.extend_from_slice(&encoded);
        self.mutate(path, Some(old), &new, MAX_STREAM, true)?;
        let published = self
            .snapshot(path, MAX_STREAM, false)?
            .ok_or_else(|| error("private ledger missing"))?;
        require(
            published.mode == 0o600,
            "private plan object requires exact private mode",
        )
    }
    pub fn append(&self, path: impl AsRef<Path>, row: &Value) -> Result<()> {
        let path = path.as_ref();
        let old = self.optional(path, MAX_STREAM)?.unwrap_or_default();
        rows(&old)?;
        let encoded = json::encoded(row)?;
        require(
            encoded.len() <= MAX_ROW,
            "plan ledger row exceeds 1048576 bytes",
        )?;
        require(
            old.len() + encoded.len() <= MAX_STREAM,
            "plan ledger exceeds read bound",
        )?;
        let a = self.anchor(path)?;
        let before = a.stat()?;
        if let Some(ref info) = before {
            self.trusted(info, false, false)?;
        }
        let fd = fs::openat(
            a.parent(),
            &a.leaf,
            OFlags::WRONLY
                | OFlags::APPEND
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io)?;
        let opened = fs::fstat(&fd).map_err(io)?;
        self.trusted(&opened, false, false)?;
        if let Some(before) = before {
            require(identity(&before, &opened), "ledger changed before append")?;
        }
        let named = a
            .stat()?
            .ok_or_else(|| error("ledger disappeared before append"))?;
        require(identity(&opened, &named), "ledger changed before append")?;
        let written = rustix::io::write(&fd, &encoded).map_err(io)?;
        require(
            written == encoded.len(),
            "ledger append was short; partial row requires reconciliation",
        )?;
        fs::fsync(&fd)
            .map_err(|e| error(format!("ledger row visible; durability uncertain: {e}")))?;
        let named = a
            .stat()?
            .ok_or_else(|| error("ledger disappeared after append"))?;
        require(
            identity(&opened, &named) && named.st_nlink == 1,
            "ledger changed during append; reconcile before retry",
        )?;
        sync_dir(a.parent())?;
        a.check()
    }
    pub fn publish_once(&self, path: impl AsRef<Path>, raw: &[u8], limit: usize) -> Result<()> {
        let path = path.as_ref();
        if let Some(old) = self.optional(path, limit)? {
            require(
                old == raw,
                "immutable authority path occupied by different bytes",
            )?;
            return self.sync(path);
        }
        self.mutate(path, Some(&[]), raw, limit, true)?;
        self.sync(path)
    }
    /// Recover only the deterministic same-inode pair left by the legacy
    /// archive/intent link publication. Other hardlinks are always refused.
    pub fn recover_pair(&self, path: impl AsRef<Path>, temporary: impl AsRef<Path>) -> Result<()> {
        let a = self.anchor(path.as_ref())?;
        let b = self.anchor(temporary.as_ref())?;
        let (Some(target), Some(tmp)) = (a.stat()?, b.stat()?) else {
            return Ok(());
        };
        require(
            identity(&target, &tmp)
                && target.st_nlink == 2
                && tmp.st_nlink == 2
                && FileType::from_raw_mode(target.st_mode) == FileType::RegularFile
                && FileType::from_raw_mode(tmp.st_mode) == FileType::RegularFile
                && target.st_uid == rustix::process::getuid().as_raw()
                && target.st_mode & 0o022 == 0,
            "interrupted authority link pair is unsafe",
        )?;
        a.check()?;
        b.check()?;
        fs::unlinkat(b.parent(), &b.leaf, AtFlags::empty()).map_err(io)?;
        sync_dir(b.parent())?;
        a.check()?;
        let repaired = a
            .stat()?
            .ok_or_else(|| error("authority link pair target disappeared"))?;
        self.trusted(&repaired, false, false)
    }
    pub fn names(
        &self,
        directory: impl AsRef<Path>,
        limit: usize,
        missing: bool,
    ) -> Result<Vec<OsString>> {
        let rel = directory.as_ref().join("probe");
        let a = match self.anchor(&rel) {
            Ok(a) => a,
            Err(e) => {
                if missing
                    && let Err(oms_safe_fs::ByteError::Missing) = self.root.read_no_follow(
                        &rel,
                        ByteReadOptions {
                            missing_ok: false,
                            max_bytes: 0,
                        },
                    )
                {
                    return Ok(vec![]);
                }
                return Err(e);
            }
        };
        let mut names = Vec::new();
        for entry in std::fs::read_dir(self.repo().join(directory)).map_err(io)? {
            require(names.len() < limit, "plan namespace entry bound exceeded")?;
            names.push(entry.map_err(io)?.file_name());
        }
        a.check()?;
        names.sort();
        Ok(names)
    }
    pub fn directory_exists(&self, directory: impl AsRef<Path>) -> Result<bool> {
        let probe = directory.as_ref().join("probe");
        match self.anchor(&probe) {
            Ok(_) => Ok(true),
            Err(e) => match self.root.read_no_follow(
                &probe,
                ByteReadOptions {
                    missing_ok: false,
                    max_bytes: 0,
                },
            ) {
                Err(oms_safe_fs::ByteError::Missing) => Ok(false),
                _ => Err(e),
            },
        }
    }
    pub fn remove(
        &self,
        path: impl AsRef<Path>,
        expected: &[u8],
        expected_generation: Option<&Value>,
    ) -> Result<()> {
        let path = path.as_ref();
        let a = self.anchor(path)?;
        let current = self
            .snapshot(path, MAX_STREAM, false)?
            .ok_or_else(|| error("missing remove target"))?;
        require(
            current.raw == expected && expected_generation.is_none_or(|g| g == &current.generation),
            "plan generation changed before unlink",
        )?;
        let s = a.stat()?.ok_or_else(|| error("missing remove target"))?;
        require(
            generation(&s) == current.generation,
            "plan generation changed before unlink",
        )?;
        fs::unlinkat(a.parent(), &a.leaf, AtFlags::empty()).map_err(io)?;
        sync_dir(a.parent())?;
        a.check()
    }
    pub(crate) fn lstat_regular(&self, path: impl AsRef<Path>) -> Result<(Value, i128)> {
        let a = self.anchor(path.as_ref())?;
        let s = a.stat()?.ok_or_else(|| error("plan object is missing"))?;
        self.trusted(&s, false, false)?;
        let g = generation(&s);
        let mtime_ns = g.get("mtime_ns").int()?;
        Ok((g, mtime_ns))
    }
    pub(crate) fn remove_generation(
        &self,
        path: impl AsRef<Path>,
        expected_generation: &Value,
    ) -> Result<()> {
        let a = self.anchor(path.as_ref())?;
        let s = a.stat()?.ok_or_else(|| error("missing remove target"))?;
        self.trusted(&s, false, false)?;
        require(
            &generation(&s) == expected_generation,
            "plan generation changed before unlink",
        )?;
        fs::unlinkat(a.parent(), &a.leaf, AtFlags::empty()).map_err(io)?;
        sync_dir(a.parent())?;
        a.check()
    }
    pub fn sync(&self, path: impl AsRef<Path>) -> Result<()> {
        let a = self.anchor(path.as_ref())?;
        let fd = fs::openat(
            a.parent(),
            &a.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        self.trusted(&fs::fstat(&fd).map_err(io)?, false, false)?;
        fs::fsync(&fd).map_err(io)?;
        sync_dir(a.parent())?;
        a.check()
    }
    pub fn private_read(&self, path: impl AsRef<Path>, limit: usize) -> Result<Vec<u8>> {
        let raw = self.read(&path, limit)?;
        let a = self.anchor(path.as_ref())?;
        self.trusted(&fs::fstat(a.parent()).map_err(io)?, true, true)?;
        self.trusted(
            &a.stat()?.ok_or_else(|| error("private object missing"))?,
            false,
            true,
        )?;
        Ok(raw)
    }
    pub fn tail(&self, path: impl AsRef<Path>, limit: usize) -> Result<(Vec<u8>, bool)> {
        use std::io::{Read, Seek, SeekFrom};
        let a = self.anchor(path.as_ref())?;
        let before = a.stat()?.ok_or_else(|| error("tail input missing"))?;
        self.trusted(&before, false, false)?;
        let fd = fs::openat(
            a.parent(),
            &a.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io)?;
        let opened = fs::fstat(&fd).map_err(io)?;
        require(stable(&before, &opened), "tail input changed while opening")?;
        let mut file = std::fs::File::from(fd);
        let length = u64::try_from(opened.st_size).map_err(|_| error("invalid tail input size"))?;
        let start = length.saturating_sub(limit as u64);
        file.seek(SeekFrom::Start(start)).map_err(io)?;
        let mut raw = Vec::new();
        file.by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(io)?;
        let after = fs::fstat(&file).map_err(io)?;
        let named = a.stat()?.ok_or_else(|| error("tail input disappeared"))?;
        require(
            raw.len() <= limit && stable(&opened, &after) && stable(&after, &named),
            "tail input changed during read",
        )?;
        Ok((raw, start == 0))
    }
}
fn sync_dir(fd: &OwnedFd) -> Result<()> {
    match fs::fsync(fd) {
        Ok(())
        | Err(rustix::io::Errno::BADF | rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => {
            Ok(())
        }
        Err(e) => Err(io(e)),
    }
}
pub fn rows(raw: &[u8]) -> Result<Vec<Value>> {
    if raw.is_empty() {
        return Ok(vec![]);
    }
    require(raw.ends_with(b"\n"), "evidence stream lacks terminal LF")?;
    raw[..raw.len() - 1]
        .split(|b| *b == b'\n')
        .map(|line| {
            require(
                !line.is_empty() && line.len() < MAX_ROW && !line.contains(&0),
                "invalid evidence row size or NUL",
            )?;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            require(!line.contains(&b'\r'), "evidence row contains bare CR")?;
            let value = json::parse(line)?;
            value.object()?;
            Ok(value)
        })
        .collect()
}
pub fn random_hex(length: usize) -> Result<String> {
    let mut bytes = vec![0u8; length];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(io)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
