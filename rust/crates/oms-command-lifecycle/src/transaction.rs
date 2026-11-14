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
    _journal_lock: oms_lock::Guard,
}
impl Transaction {
    pub fn begin(paths: &Paths) -> Result<Self> {
        let dir = paths.config.join("native-lifecycle");
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
        let old = identity(&target)?;
        prepare(&staged)?;
        let new = identity(&staged)?;
        self.journal.changes.push(Change {
            target,
            staged,
            undo,
            old,
            new,
        });
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if identity(&change.target)? != change.old {
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
        let target = r::leaf(target)?;
        let old = identity(&target)?;
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
        });
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if identity(&change.target)? != change.old {
            return r::fail("target changed before removal");
        }
        fs::rename(&change.target, &change.undo)?;
        r::sync_dir(change.target.parent().unwrap())?;
        Ok(())
    }
    pub fn effect(&mut self, effect: crate::effects::Effect) -> Result<()> {
        self.journal.effects.push(effect);
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
        let current = identity(&c.target)?;
        let undo = identity(&c.undo)?;
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
            if identity(&c.staged)? != c.new {
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
            if identity(&c.undo)? != c.old {
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
