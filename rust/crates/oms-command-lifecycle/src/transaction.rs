use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct Change {
    target: PathBuf,
    staged: PathBuf,
    undo: PathBuf,
    old: Option<String>,
    new: Option<String>,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    parent_identity: Option<(u64, u64)>,
}
impl Change {
    fn identity(&self, path: &Path) -> Result<Option<String>> {
        if self.external {
            external_identity(path)
        } else {
            identity(path)
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Journal {
    schema: u32,
    committed: bool,
    changes: Vec<Change>,
    #[serde(default)]
    effects: Vec<crate::effects::Effect>,
}
/// Write-ahead journal. Callers hold the install receipt's canonical lock.
/// Refusals retain the journal and undo names when recovery cannot prove identity.
pub(crate) struct Transaction {
    path: PathBuf,
    journal: Journal,
    complete: bool,
    notices: Vec<String>,
    _journal_lock: oms_lock::Guard,
}
impl Transaction {
    pub fn begin(paths: &Paths) -> Result<Self> {
        Self::begin_named(paths, "native-lifecycle")
    }
    pub fn begin_update(paths: &Paths) -> Result<Self> {
        Self::begin_named(paths, "native-update")
    }
    fn begin_named(paths: &Paths, name: &str) -> Result<Self> {
        let dir = paths.config.join(name);
        let path = dir.join("transaction.json");
        // Receipt overrides may differ while sharing this recovery journal.
        let journal_lock = r::lock(&path, &paths.cwd)?;
        r::private_dir(&dir)?;
        if r::exists(&path) {
            Self::recover(&path)?;
        }
        let tx = Self {
            path,
            journal: Journal {
                schema: 1,
                committed: false,
                changes: Vec::new(),
                effects: Vec::new(),
            },
            complete: false,
            notices: Vec::new(),
            _journal_lock: journal_lock,
        };
        tx.save()?;
        Ok(tx)
    }
    fn save(&self) -> Result<()> {
        let value = serde_json::to_value(&self.journal)
            .map_err(|_| r::Error(2, "cannot serialize lifecycle journal".into()))?;
        r::atomic(&self.path, &r::bytes(&value)?)
    }
    pub fn write(&mut self, target: &Path, raw: &[u8], mode: u32) -> Result<()> {
        self.replace(target, |p| r::new_file(p, raw, mode))
    }
    pub fn link(&mut self, target: &Path, source: &Path) -> Result<()> {
        self.replace(target, |p| {
            symlink(source, p)?;
            Ok(())
        })
    }
    pub fn copy(&mut self, target: &Path, source: &Path) -> Result<()> {
        self.replace(target, |p| copy(source, p))
    }
    pub fn files(&mut self, target: &Path, files: &[(&str, &[u8], u32)]) -> Result<()> {
        self.replace(target, |p| {
            fs::create_dir(p)?;
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
            for (name, raw, mode) in files {
                if !r::safe_name(name) || matches!(*name, "." | "..") {
                    return r::fail("invalid staged payload member");
                }
                r::new_file(&p.join(name), raw, *mode)?;
            }
            r::sync_dir(p)
        })
    }
    fn replace(&mut self, target: &Path, prepare: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
        self.replace_mode(target, false, prepare)
    }
    fn replace_mode(
        &mut self,
        target: &Path,
        external: bool,
        prepare: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        let identify = |p: &Path| {
            if external {
                external_identity(p)
            } else {
                identity(p)
            }
        };
        let target = r::leaf(target)?;
        let parent = target.parent().unwrap();
        fs::create_dir_all(parent)?;
        let target = r::leaf(&target)?;
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let suffix = format!("{}.{}.{}", std::process::id(), r::now().replace(':', ""), n);
        let staged = target.with_file_name(format!(".oms-stage-{suffix}"));
        let undo = target.with_file_name(format!(".oms-undo-{suffix}"));
        if r::exists(&staged) || r::exists(&undo) {
            return r::fail("transaction staging name already exists");
        }
        let old = identify(&target)?;
        prepare(&staged)?;
        let new = identify(&staged)?;
        self.journal.changes.push(Change {
            target,
            staged,
            undo,
            old,
            new,
            external,
            parent_identity: None,
        });
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if change.identity(&change.target)? != change.old {
            return r::fail("target changed while staging lifecycle mutation");
        }
        if change.old.is_some() {
            fs::rename(&change.target, &change.undo)?;
            r::sync_dir(change.target.parent().unwrap())?;
        }
        fs::rename(&change.staged, &change.target)?;
        r::sync_dir(change.target.parent().unwrap())?;
        Ok(())
    }
    pub fn remove(&mut self, target: &Path) -> Result<()> {
        self.remove_mode(target, false)
    }
    pub fn remove_external(&mut self, target: &Path) -> Result<()> {
        self.remove_mode(target, true)
    }
    fn remove_mode(&mut self, target: &Path, external: bool) -> Result<()> {
        let target = r::leaf(target)?;
        let old = if external {
            external_identity(&target)?
        } else {
            identity(&target)?
        };
        if old.is_none() {
            return Ok(());
        }
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let undo = target.with_file_name(format!(".oms-undo-{}.{}", std::process::id(), n));
        if r::exists(&undo) {
            return r::fail("transaction undo name already exists");
        }
        self.journal.changes.push(Change {
            staged: undo.with_extension("stage"),
            undo,
            target,
            old,
            new: None,
            external,
            parent_identity: None,
        });
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if change.identity(&change.target)? != change.old {
            return r::fail("target changed before removal");
        }
        fs::rename(&change.target, &change.undo)?;
        r::sync_dir(change.target.parent().unwrap())?;
        Ok(())
    }
    pub fn write_bound(
        &mut self,
        target: &Path,
        raw: &[u8],
        mode: u32,
        parent: &serde_json::Value,
        expected: Option<&[u8]>,
        expected_identity: Option<&serde_json::Value>,
    ) -> Result<()> {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        let root = target
            .parent()
            .ok_or_else(|| r::Error(2, "invalid account target".into()))?;
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(root)?;
        let m = directory.metadata()?;
        let check = || -> Result<()> {
            let now = fs::symlink_metadata(root)?;
            if now.file_type().is_symlink()
                || parent["st_dev"] != m.dev()
                || parent["st_ino"] != m.ino()
                || now.dev() != m.dev()
                || now.ino() != m.ino()
            {
                return r::fail("account settings home changed; retained");
            }
            Ok(())
        };
        check()?;
        let name = target
            .file_name()
            .ok_or_else(|| r::Error(2, "invalid account settings name".into()))?;
        let old = match rustix::fs::openat(
            &directory,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => {
                let file = fs::File::from(fd);
                let info = file.metadata()?;
                if !info.is_file()
                    || info.nlink() != 1
                    || info.len() > r::LIMIT as u64
                    || expected_identity
                        .is_some_and(|id| id["st_dev"] != info.dev() || id["st_ino"] != info.ino())
                {
                    return r::fail("account settings identity changed; retained");
                }
                let mut before = Vec::new();
                (&file).take(r::LIMIT as u64 + 1).read_to_end(&mut before)?;
                if expected != Some(before.as_slice()) {
                    return r::fail("account settings changed concurrently; retained");
                }
                let mut hash = Sha256::new();
                hash.update(b"file\0");
                hash.update(before);
                Some(format!("{:x}", hash.finalize()))
            }
            Err(rustix::io::Errno::NOENT) if expected.is_none() => None,
            _ => return r::fail("account settings target changed; retained"),
        };
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let staged = target.with_file_name(format!(".oms-stage-{}.{}", std::process::id(), n));
        let undo = target.with_file_name(format!(".oms-undo-{}.{}", std::process::id(), n));
        if r::exists(&undo) {
            return r::fail("account settings undo name exists");
        }
        #[cfg(target_os = "macos")]
        let raw_mode = mode as u16;
        #[cfg(not(target_os = "macos"))]
        let raw_mode = mode;
        let fd = rustix::fs::openat(
            &directory,
            staged.file_name().unwrap(),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(raw_mode),
        )
        .map_err(|_| r::Error(2, "cannot stage bound account settings".into()))?;
        let mut output = fs::File::from(fd);
        output.write_all(raw)?;
        output.sync_all()?;
        let mut hash = Sha256::new();
        hash.update(b"file\0");
        hash.update(raw);
        let new = Some(format!("{:x}", hash.finalize()));
        self.journal.changes.push(Change {
            target: target.to_owned(),
            staged: staged.clone(),
            undo: undo.clone(),
            old: old.clone(),
            new,
            external: false,
            parent_identity: Some((m.dev(), m.ino())),
        });
        self.save()?;
        check()?;
        if identity(target)? != old {
            return r::fail("account settings changed before publication");
        }
        if old.is_some() {
            rustix::fs::renameat(&directory, name, &directory, undo.file_name().unwrap())
                .map_err(|_| r::Error(2, "account settings backup failed".into()))?;
        }
        rustix::fs::renameat(&directory, staged.file_name().unwrap(), &directory, name)
            .map_err(|_| r::Error(2, "account settings publication failed".into()))?;
        directory.sync_all()?;
        check()
    }
    pub fn remove_link_bound(
        &mut self,
        target: &Path,
        parent: &serde_json::Value,
        entry: &serde_json::Value,
        destination: &Path,
    ) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = target
            .parent()
            .ok_or_else(|| r::Error(2, "invalid account target".into()))?;
        let handle = fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(directory)?;
        let m = handle.metadata()?;
        if parent["st_dev"] != m.dev() || parent["st_ino"] != m.ino() {
            return r::fail("account home changed before removal");
        }
        let name = target
            .file_name()
            .ok_or_else(|| r::Error(2, "invalid account link name".into()))?;
        let inspect = || -> Result<()> {
            let st = rustix::fs::statat(&handle, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| r::Error(2, "account link disappeared".into()))?;
            let to = rustix::fs::readlinkat(&handle, name, Vec::new())
                .map_err(|_| r::Error(2, "account entry is no longer a link".into()))?;
            if entry["st_dev"] != st.st_dev
                || entry["st_ino"] != st.st_ino
                || to.as_bytes() != destination.as_os_str().as_bytes()
            {
                return r::fail("account link identity changed; retained");
            }
            let current = fs::symlink_metadata(directory)?;
            if current.file_type().is_symlink()
                || current.dev() != m.dev()
                || current.ino() != m.ino()
            {
                return r::fail("account home changed before removal");
            }
            Ok(())
        };
        inspect()?;
        let mut hash = Sha256::new();
        hash.update(b"link\0");
        hash.update(destination.as_os_str().as_bytes());
        let old = Some(format!("{:x}", hash.finalize()));
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let undo = target.with_file_name(format!(".oms-undo-{}.{}", std::process::id(), n));
        if r::exists(&undo) {
            return r::fail("account undo destination exists");
        }
        self.journal.changes.push(Change {
            target: target.to_owned(),
            staged: undo.with_extension("stage"),
            undo: undo.clone(),
            old,
            new: None,
            external: false,
            parent_identity: Some((m.dev(), m.ino())),
        });
        self.save()?;
        inspect()?;
        rustix::fs::renameat(&handle, name, &handle, undo.file_name().unwrap())
            .map_err(|_| r::Error(2, "account unlink transaction failed".into()))?;
        handle.sync_all()?;
        Ok(())
    }
    pub fn effect(&mut self, effect: crate::effects::Effect) -> Result<()> {
        self.journal.effects.push(effect);
        self.save()
    }
    pub fn warn(&mut self, message: &str) {
        self.notices.push(format!("warning: {message}\n"));
    }
    pub fn diagnostics(&self) -> Vec<u8> {
        self.notices.concat().into_bytes()
    }
    /// Snapshot provider-owned files before letting its CLI rewrite them. A kill
    /// before sealing retains the undo copy and refuses uncertain recovery.
    pub fn publish_external(&mut self, target: &Path, source: &Path) -> Result<()> {
        self.replace_mode(target, true, |p| external_copy(source, p))
    }
    pub fn prepare_external(&mut self, targets: &[PathBuf]) -> Result<usize> {
        let start = self.journal.changes.len();
        for target in targets {
            let target = r::leaf(target)?;
            fs::create_dir_all(target.parent().unwrap())?;
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let undo = target.with_file_name(format!(".oms-undo-{}.{}", std::process::id(), n));
            let staged = target.with_file_name(format!(".oms-stage-{}.{}", std::process::id(), n));
            if r::exists(&undo) || r::exists(&staged) {
                return r::fail("external snapshot name already exists");
            }
            let old = external_identity(&target)?;
            self.journal.changes.push(Change {
                target: target.clone(),
                staged,
                undo: undo.clone(),
                old: old.clone(),
                new: old.clone(),
                external: true,
                parent_identity: None,
            });
            self.save()?;
            if old.is_some() {
                external_copy(&target, &undo)?;
                if external_identity(&undo)? != old || external_identity(&target)? != old {
                    return r::fail("provider state changed during snapshot");
                }
                r::sync_dir(target.parent().unwrap())?;
            }
        }
        Ok(start)
    }
    pub fn finish_external(&mut self, start: usize) -> Result<()> {
        for change in &mut self.journal.changes[start..] {
            change.new = change.identity(&change.target)?;
        }
        self.save()
    }
    pub fn commit(mut self) -> Result<()> {
        self.journal.committed = true;
        self.save()?;
        self.complete = true;
        cleanup(&self.journal)?;
        fs::remove_file(&self.path)?;
        r::sync_dir(self.path.parent().unwrap())?;
        Ok(())
    }
    fn recover(path: &Path) -> Result<()> {
        let v = r::value(path)?;
        let journal: Journal = serde_json::from_value(v)
            .map_err(|_| r::Error(2, "invalid lifecycle recovery journal".into()))?;
        if journal.schema != 1 {
            return r::fail("unknown lifecycle recovery schema");
        }
        validate(&journal)?;
        if journal.committed {
            cleanup(&journal)?;
        } else {
            rollback(&journal)?;
        }
        fs::remove_file(path)?;
        r::sync_dir(path.parent().unwrap())?;
        Ok(())
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.complete && rollback(&self.journal).is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn validate(j: &Journal) -> Result<()> {
    for c in &j.changes {
        if let Some((dev, ino)) = c.parent_identity {
            let m = fs::symlink_metadata(
                c.target
                    .parent()
                    .ok_or_else(|| r::Error(2, "invalid bound target".into()))?,
            )?;
            if !m.is_dir() || m.file_type().is_symlink() || m.dev() != dev || m.ino() != ino {
                return r::fail("account parent moved; recovery journal retained");
            }
        }
        let parent = c
            .target
            .parent()
            .ok_or_else(|| r::Error(2, "invalid journal target".into()))?;
        if !c.target.is_absolute()
            || c.target.file_name().is_none()
            || c.staged.parent() != Some(parent)
            || c.undo.parent() != Some(parent)
            || !c
                .staged
                .file_name()
                .unwrap_or_default()
                .as_bytes()
                .starts_with(b".oms-")
            || !c
                .undo
                .file_name()
                .unwrap_or_default()
                .as_bytes()
                .starts_with(b".oms-undo-")
        {
            return r::fail("unsafe lifecycle recovery paths");
        }
    }
    Ok(())
}
fn rollback(j: &Journal) -> Result<()> {
    validate(j)?;
    for c in j.changes.iter().rev() {
        let current = c.identity(&c.target)?;
        let undo = c.identity(&c.undo)?;
        if c.external && current == c.old && undo == c.old && undo.is_some() {
            remove(&c.undo)?;
            if r::exists(&c.staged) {
                return r::fail("unexpected external staging during rollback");
            }
            r::sync_dir(c.target.parent().unwrap())?;
            continue;
        }
        if undo.is_some() {
            if undo != c.old || (current.is_some() && current != c.new) {
                return r::fail("rollback conflict; lifecycle recovery journal retained");
            }
            remove(&c.target)?;
            fs::rename(&c.undo, &c.target)?;
        } else if c.old.is_none() {
            if current.is_some() && current != c.new {
                return r::fail("new lifecycle target changed; recovery journal retained");
            }
            remove(&c.target)?;
        } else if current != c.old {
            return r::fail("previous lifecycle target missing; recovery journal retained");
        }
        if r::exists(&c.staged) {
            if c.identity(&c.staged)? != c.new {
                return r::fail("staging content changed; recovery journal retained");
            }
            remove(&c.staged)?;
        }
        r::sync_dir(c.target.parent().unwrap())?;
    }
    for effect in j.effects.iter().rev() {
        effect.restore()?;
    }
    Ok(())
}
fn cleanup(j: &Journal) -> Result<()> {
    for c in &j.changes {
        if r::exists(&c.undo) {
            if c.identity(&c.undo)? != c.old {
                return r::fail("undo content changed; recovery journal retained");
            }
            remove(&c.undo)?;
        }
        if r::exists(&c.staged) {
            return r::fail("unexpected lifecycle staging content");
        }
        r::sync_dir(c.target.parent().unwrap())?;
    }
    Ok(())
}
pub(crate) fn remove(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub(crate) fn identity(path: &Path) -> Result<Option<String>> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut h = Sha256::new();
    if m.file_type().is_symlink() {
        h.update(b"link\0");
        h.update(fs::read_link(path)?.as_os_str().as_bytes());
    } else if m.is_file() {
        if m.nlink() != 1 {
            return r::fail("multiply linked lifecycle file");
        }
        h.update(b"file\0");
        h.update(r::read(path, 1024 * 1024 * 1024)?);
    } else if m.is_dir() {
        h.update(b"directory\0");
        fn visit(base: &Path, path: &Path, h: &mut Sha256, depth: usize) -> Result<()> {
            if depth > 128 {
                return r::fail("directory nesting exceeds lifecycle limit");
            }
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|a| a.file_name());
            for e in entries
                .iter()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            {
                h.update(b"D\0");
                h.update(e.path().strip_prefix(base).unwrap().as_os_str().as_bytes());
                h.update(b"\0");
            }
            for e in &entries {
                let m = fs::symlink_metadata(e.path())?;
                if m.is_dir() {
                    continue;
                }
                if !m.is_file() || m.nlink() != 1 {
                    return r::fail("managed directory contains a nonregular entry");
                }
                h.update(b"F\0");
                h.update(e.path().strip_prefix(base).unwrap().as_os_str().as_bytes());
                h.update(b"\0");
                h.update(r::read(&e.path(), 1024 * 1024 * 1024)?);
                h.update(b"\0");
            }
            for e in entries
                .iter()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            {
                visit(base, &e.path(), h, depth + 1)?;
            }
            Ok(())
        }
        visit(path, path, &mut h, 0)?;
    } else {
        return r::fail("unsupported lifecycle target type");
    }
    Ok(Some(format!("{:x}", h.finalize())))
}
fn copy(source: &Path, target: &Path) -> Result<()> {
    let m = fs::symlink_metadata(source)?;
    if m.file_type().is_symlink() {
        return r::fail("managed copies cannot contain symlinks");
    }
    if m.is_file() {
        r::new_file(
            target,
            &r::read(source, 1024 * 1024 * 1024)?,
            m.permissions().mode() & 0o777,
        )?;
    } else if m.is_dir() {
        fs::create_dir(target)?;
        for e in fs::read_dir(source)? {
            let e = e?;
            copy(&e.path(), &target.join(e.file_name()))?;
        }
        r::sync_dir(target)?;
    } else {
        return r::fail("managed copy source is not a file or directory");
    }
    Ok(())
}

pub(crate) fn external_read(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > 1024 * 1024 * 1024 {
        return r::fail("external snapshot requires a bounded regular file");
    }
    let mut raw = Vec::new();
    (&file).take(1024 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
    let after = file.metadata()?;
    if raw.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return r::fail("provider file changed during snapshot");
    }
    Ok(raw)
}
fn external_identity(path: &Path) -> Result<Option<String>> {
    fn visit(path: &Path, h: &mut Sha256, depth: usize) -> Result<()> {
        if depth > 128 {
            return r::fail("external snapshot nesting exceeds limit");
        }
        let m = fs::symlink_metadata(path)?;
        h.update(m.mode().to_le_bytes());
        if m.file_type().is_symlink() {
            h.update(b"link");
            h.update(fs::read_link(path)?.as_os_str().as_bytes());
        } else if m.is_file() {
            h.update(b"file");
            h.update(external_read(path)?);
        } else if m.is_dir() {
            h.update(b"dir");
            let mut rows = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            rows.sort_by_key(|e| e.file_name());
            for row in rows {
                h.update(row.file_name().as_bytes());
                h.update(b"\0");
                visit(&row.path(), h, depth + 1)?;
            }
        } else {
            return r::fail("provider snapshot refuses special files");
        }
        Ok(())
    }
    if !r::exists(path) {
        return Ok(None);
    }
    let mut h = Sha256::new();
    visit(path, &mut h, 0)?;
    Ok(Some(format!("{:x}", h.finalize())))
}
fn external_copy(source: &Path, target: &Path) -> Result<()> {
    let m = fs::symlink_metadata(source)?;
    if m.file_type().is_symlink() {
        symlink(fs::read_link(source)?, target)?;
    } else if m.is_file() {
        r::new_file(target, &external_read(source)?, m.mode() & 0o777)?;
    } else if m.is_dir() {
        fs::create_dir(target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(m.mode() & 0o777))?;
        for row in fs::read_dir(source)? {
            let row = row?;
            external_copy(&row.path(), &target.join(row.file_name()))?;
        }
        r::sync_dir(target)?;
    } else {
        return r::fail("provider snapshot refuses special files");
    }
    Ok(())
}
