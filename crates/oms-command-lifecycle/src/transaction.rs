use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
const SKILL_FILE: &str = "SKILL.md";
const HIDDEN_SKILL_FILE: &str = ".oms-hidden-SKILL.md";

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
    #[serde(default)]
    hidden_old_skill: bool,
    #[serde(default)]
    hidden_new_skill: bool,
    #[serde(default)]
    old_link: Option<Vec<u8>>,
    #[serde(default)]
    old_link_mode: Option<u32>,
}
impl Change {
    fn identity(&self, path: &Path, allow_hidden_skill: bool) -> Result<Option<String>> {
        if self.external {
            external_identity_with_hidden(path, allow_hidden_skill)
        } else {
            identity_with_hidden(path, allow_hidden_skill)
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
        self.replace(target, |p| {
            r::new_file(p, raw, mode)?;
            Ok(false)
        })
    }
    pub fn link(&mut self, target: &Path, source: &Path) -> Result<()> {
        self.replace(target, |p| {
            symlink(source, p)?;
            Ok(false)
        })
    }
    pub fn copy(&mut self, target: &Path, source: &Path) -> Result<()> {
        self.replace(target, |p| {
            copy(source, p, Some((SKILL_FILE, HIDDEN_SKILL_FILE)))
        })
    }
    pub fn files(&mut self, target: &Path, files: &[(&str, &[u8], u32)]) -> Result<()> {
        self.files_mode(target, files, false)
    }
    pub fn create_files(&mut self, target: &Path, files: &[(&str, &[u8], u32)]) -> Result<()> {
        self.files_mode(target, files, true)
    }
    fn files_mode(
        &mut self,
        target: &Path,
        files: &[(&str, &[u8], u32)],
        create_only: bool,
    ) -> Result<()> {
        self.replace_mode(target, false, create_only, |p| {
            fs::create_dir(p)?;
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
            let hide = !files.iter().any(|(name, _, _)| *name == HIDDEN_SKILL_FILE);
            let mut hid = false;
            for (name, raw, mode) in files {
                if !r::safe_name(name) || matches!(*name, "." | "..") {
                    return r::fail("invalid staged payload member");
                }
                let dest_name = if hide && *name == SKILL_FILE {
                    hid = true;
                    HIDDEN_SKILL_FILE
                } else {
                    *name
                };
                r::new_file(&p.join(dest_name), raw, *mode)?;
            }
            r::sync_dir(p)?;
            Ok(hid)
        })
    }
    fn replace(
        &mut self,
        target: &Path,
        prepare: impl FnOnce(&Path) -> Result<bool>,
    ) -> Result<()> {
        self.replace_mode(target, false, false, prepare)
    }
    fn replace_mode(
        &mut self,
        target: &Path,
        external: bool,
        create_only: bool,
        prepare: impl FnOnce(&Path) -> Result<bool>,
    ) -> Result<()> {
        let identify = |p: &Path, allow_hidden: bool| {
            if external {
                external_identity_with_hidden(p, allow_hidden)
            } else {
                identity_with_hidden(p, allow_hidden)
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
        let old = identify(&target, false)?;
        if create_only && old.is_some() {
            return r::fail("refusing to replace an existing lifecycle target");
        }
        let (old_link, old_link_mode) = read_old_link(&target)?;
        let hidden_old_skill = if old.is_some() {
            can_hide_skill(&target)?
        } else {
            false
        };
        let hidden_new_skill = prepare(&staged)?;
        let new = identify(&staged, hidden_new_skill)?;
        self.journal.changes.push(Change {
            target,
            staged,
            undo,
            old,
            new,
            external,
            parent_identity: None,
            hidden_old_skill,
            hidden_new_skill,
            old_link,
            old_link_mode,
        });
        validate(&self.journal)?;
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if change.identity(&change.target, false)? != change.old {
            return r::fail("target changed while staging lifecycle mutation");
        }
        if change.old.is_some() && change.old_link.is_none() {
            fs::rename(&change.target, &change.undo)?;
            if change.hidden_old_skill {
                hide_skill(&change.undo)?;
            }
            r::sync_dir(change.target.parent().unwrap())?;
        }
        if change.old_link.is_some() && fs::symlink_metadata(&change.staged)?.is_dir() {
            fs::remove_file(&change.target)?;
        }
        fs::rename(&change.staged, &change.target)?;
        if change.hidden_new_skill {
            unhide_skill(&change.target)?;
        }
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
        let (old_link, old_link_mode) = read_old_link(&target)?;
        let hidden_old_skill = can_hide_skill(&target)?;
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
            hidden_old_skill,
            hidden_new_skill: false,
            old_link,
            old_link_mode,
        });
        validate(&self.journal)?;
        self.save()?;
        let change = self.journal.changes.last().unwrap();
        if change.identity(&change.target, false)? != change.old {
            return r::fail("target changed before removal");
        }
        if change.old_link.is_some() {
            fs::remove_file(&change.target)?;
        } else {
            fs::rename(&change.target, &change.undo)?;
        }
        if change.hidden_old_skill {
            hide_skill(&change.undo)?;
        }
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
            hidden_old_skill: false,
            hidden_new_skill: false,
            old_link: None,
            old_link_mode: None,
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
            hidden_old_skill: false,
            hidden_new_skill: false,
            old_link: Some(destination.as_os_str().as_bytes().to_vec()),
            old_link_mode: Some(fs::symlink_metadata(target)?.mode()),
        });
        self.save()?;
        inspect()?;
        rustix::fs::unlinkat(&handle, name, rustix::fs::AtFlags::empty())
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
        self.replace_mode(target, true, false, |p| {
            external_copy(source, p, Some((SKILL_FILE, HIDDEN_SKILL_FILE)))
        })
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
            let (old_link, old_link_mode) = read_old_link(&target)?;
            let hidden_old_skill = if old.is_some() {
                can_hide_skill(&target)?
            } else {
                false
            };
            self.journal.changes.push(Change {
                target: target.clone(),
                staged,
                undo: undo.clone(),
                old: old.clone(),
                new: old.clone(),
                external: true,
                parent_identity: None,
                hidden_old_skill,
                hidden_new_skill: false,
                old_link,
                old_link_mode,
            });
            validate(&self.journal)?;
            self.save()?;
            if self.journal.changes.last().unwrap().old_link.is_some()
                && external_identity(&target)? != old
            {
                return r::fail("provider state changed during snapshot");
            }
            if old.is_some() && self.journal.changes.last().unwrap().old_link.is_none() {
                let hid = external_copy(&target, &undo, Some((SKILL_FILE, HIDDEN_SKILL_FILE)))?;
                if hid != hidden_old_skill
                    || external_identity_with_hidden(&undo, hidden_old_skill)? != old
                    || external_identity(&target)? != old
                {
                    return r::fail("provider state changed during snapshot");
                }
                r::sync_dir(target.parent().unwrap())?;
            }
        }
        Ok(start)
    }
    pub fn finish_external(&mut self, start: usize) -> Result<()> {
        for change in &mut self.journal.changes[start..] {
            change.new = change.identity(&change.target, false)?;
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
        if let Some(link) = &c.old_link {
            let mut hash = Sha256::new();
            if c.external {
                let mode = c
                    .old_link_mode
                    .ok_or_else(|| r::Error(2, "invalid journal link mode".into()))?;
                hash.update(mode.to_le_bytes());
                hash.update(b"link");
            } else {
                hash.update(b"link\0");
            }
            hash.update(link);
            if c.old.as_deref() != Some(format!("{:x}", hash.finalize()).as_str())
                || link.is_empty()
                || link.contains(&0)
                || c.hidden_old_skill
            {
                return r::fail("invalid lifecycle journal link");
            }
        } else if c.old_link_mode.is_some() {
            return r::fail("invalid lifecycle journal link mode");
        }
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
        let current_old = c.identity(&c.target, false)?;
        let current_new = if c.hidden_new_skill {
            c.identity(&c.target, true)?
        } else {
            current_old.clone()
        };
        let undo = c.identity(&c.undo, c.hidden_old_skill)?;
        if c.external
            && c.old_link.is_none()
            && current_old == c.old
            && undo == c.old
            && undo.is_some()
        {
            remove(&c.undo)?;
            if r::exists(&c.staged) {
                return r::fail("unexpected external staging during rollback");
            }
            r::sync_dir(c.target.parent().unwrap())?;
            continue;
        }
        if let Some(link) = &c.old_link {
            if undo.is_some() {
                return r::fail("unexpected lifecycle undo content");
            }
            if current_old != c.old {
                if current_new.is_some() && current_new != c.new {
                    return r::fail("rollback conflict; lifecycle recovery journal retained");
                }
                remove(&c.target)?;
                symlink(OsString::from_vec(link.clone()), &c.target)?;
                if c.identity(&c.target, false)? != c.old {
                    return r::fail("restored link identity changed; recovery journal retained");
                }
            }
        } else if undo.is_some() {
            if undo != c.old || (current_new.is_some() && current_new != c.new) {
                return r::fail("rollback conflict; lifecycle recovery journal retained");
            }
            remove(&c.target)?;
            if c.hidden_old_skill {
                unhide_skill(&c.undo)?;
            }
            fs::rename(&c.undo, &c.target)?;
        } else if c.old.is_none() {
            if current_new.is_some() && current_new != c.new {
                return r::fail("new lifecycle target changed; recovery journal retained");
            }
            remove(&c.target)?;
        } else if current_old != c.old {
            return r::fail("previous lifecycle target missing; recovery journal retained");
        }
        if r::exists(&c.staged) {
            if c.identity(&c.staged, c.hidden_new_skill)? != c.new {
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
            if c.old_link.is_some() {
                return r::fail("unexpected lifecycle undo content");
            }
            if c.identity(&c.undo, c.hidden_old_skill)? != c.old {
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
fn read_old_link(path: &Path) -> Result<(Option<Vec<u8>>, Option<u32>)> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Ok((
            Some(fs::read_link(path)?.as_os_str().as_bytes().to_vec()),
            Some(m.mode()),
        )),
        Ok(_) => Ok((None, None)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((None, None)),
        Err(e) => Err(e.into()),
    }
}

fn can_hide_skill(dir: &Path) -> Result<bool> {
    let m = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !m.is_dir() || m.file_type().is_symlink() {
        return Ok(false);
    }
    let skill = match fs::symlink_metadata(dir.join(SKILL_FILE)) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if skill.is_dir() {
        return Ok(false);
    }
    Ok(!r::exists(&dir.join(HIDDEN_SKILL_FILE)))
}
fn rename_entry_noreplace(dir: &fs::File, from: &str, to: &str) -> Result<()> {
    match rustix::fs::renameat_with(dir, from, dir, to, rustix::fs::RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::EXIST) => {
            r::fail("skill visibility transition destination already exists")
        }
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::NOTSUP) => {
            match rustix::fs::statat(dir, to, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => return r::fail("skill visibility transition destination already exists"),
                Err(rustix::io::Errno::NOENT) => {}
                Err(e) => return Err(std::io::Error::from(e).into()),
            }
            rustix::fs::renameat(dir, from, dir, to)
                .map_err(|e| r::Error::from(std::io::Error::from(e)))
        }
        Err(e) => Err(std::io::Error::from(e).into()),
    }
}
fn rename_skill_entry(dir: &Path, from: &str, to: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let m = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !m.is_dir() || m.file_type().is_symlink() {
        return Ok(());
    }
    let dir_file = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(dir)?;
    let src_stat = match rustix::fs::statat(&dir_file, from, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
    {
        Ok(st) => st,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(std::io::Error::from(e).into()),
    };
    if rustix::fs::FileType::from_raw_mode(src_stat.st_mode) == rustix::fs::FileType::Directory {
        return Ok(());
    }
    let orig_mode = dir_file.metadata()?.mode() & 0o7777;
    let writable = orig_mode & 0o200 != 0;
    if !writable {
        dir_file.set_permissions(fs::Permissions::from_mode(orig_mode | 0o200))?;
    }
    let res = rename_entry_noreplace(&dir_file, from, to)
        .and_then(|()| dir_file.sync_all().map_err(r::Error::from));
    if !writable {
        let restore = dir_file
            .set_permissions(fs::Permissions::from_mode(orig_mode))
            .map_err(r::Error::from);
        res?;
        return restore;
    }
    res
}
fn hide_skill(dir: &Path) -> Result<()> {
    rename_skill_entry(dir, SKILL_FILE, HIDDEN_SKILL_FILE)
}
fn unhide_skill(dir: &Path) -> Result<()> {
    rename_skill_entry(dir, HIDDEN_SKILL_FILE, SKILL_FILE)
}
fn remap_hidden_skill_entries(
    raw_entries: Vec<fs::DirEntry>,
    allow_hidden_skill: bool,
) -> Result<Vec<(OsString, fs::DirEntry)>> {
    let has_skill = raw_entries.iter().any(|e| e.file_name() == SKILL_FILE);
    let mut entries = Vec::with_capacity(raw_entries.len());
    for e in raw_entries {
        let name = e.file_name();
        let logical = if allow_hidden_skill
            && !has_skill
            && name == HIDDEN_SKILL_FILE
            && !fs::symlink_metadata(e.path())?.is_dir()
        {
            OsString::from(SKILL_FILE)
        } else {
            name
        };
        entries.push((logical, e));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
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
    identity_with_hidden(path, false)
}
fn identity_with_hidden(path: &Path, allow_hidden_skill: bool) -> Result<Option<String>> {
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
        fn visit(
            base: &Path,
            path: &Path,
            h: &mut Sha256,
            depth: usize,
            allow_hidden_skill: bool,
        ) -> Result<()> {
            if depth > 128 {
                return r::fail("directory nesting exceeds lifecycle limit");
            }
            let raw_entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            let entries =
                remap_hidden_skill_entries(raw_entries, depth == 0 && allow_hidden_skill)?;
            for (logical, e) in entries
                .iter()
                .filter(|(_, e)| e.file_type().is_ok_and(|t| t.is_dir()))
            {
                h.update(b"D\0");
                if depth == 0 {
                    h.update(logical.as_bytes());
                } else {
                    h.update(e.path().strip_prefix(base).unwrap().as_os_str().as_bytes());
                }
                h.update(b"\0");
            }
            for (logical, e) in &entries {
                let m = fs::symlink_metadata(e.path())?;
                if m.is_dir() {
                    continue;
                }
                if !m.is_file() || m.nlink() != 1 {
                    return r::fail("managed directory contains a nonregular entry");
                }
                h.update(b"F\0");
                if depth == 0 {
                    h.update(logical.as_bytes());
                } else {
                    h.update(e.path().strip_prefix(base).unwrap().as_os_str().as_bytes());
                }
                h.update(b"\0");
                h.update(r::read(&e.path(), 1024 * 1024 * 1024)?);
                h.update(b"\0");
            }
            for (_, e) in entries
                .iter()
                .filter(|(_, e)| e.file_type().is_ok_and(|t| t.is_dir()))
            {
                visit(base, &e.path(), h, depth + 1, false)?;
            }
            Ok(())
        }
        visit(path, path, &mut h, 0, allow_hidden_skill)?;
    } else {
        return r::fail("unsupported lifecycle target type");
    }
    Ok(Some(format!("{:x}", h.finalize())))
}
fn copy(source: &Path, target: &Path, rename_top: Option<(&str, &str)>) -> Result<bool> {
    let m = fs::symlink_metadata(source)?;
    if m.file_type().is_symlink() {
        return r::fail("managed copies cannot contain symlinks");
    }
    let mut renamed = false;
    if m.is_file() {
        r::new_file(
            target,
            &r::read(source, 1024 * 1024 * 1024)?,
            m.permissions().mode() & 0o777,
        )?;
    } else if m.is_dir() {
        let active_rename = rename_top.filter(|(_, to)| !r::exists(&source.join(to)));
        fs::create_dir(target)?;
        for e in fs::read_dir(source)? {
            let e = e?;
            let name = e.file_name();
            let dest = if let Some((from, to)) = active_rename
                && name == from
                && !fs::symlink_metadata(e.path())?.is_dir()
            {
                renamed = true;
                target.join(to)
            } else {
                target.join(&name)
            };
            copy(&e.path(), &dest, None)?;
        }
        r::sync_dir(target)?;
    } else {
        return r::fail("managed copy source is not a file or directory");
    }
    Ok(renamed)
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
    external_identity_with_hidden(path, false)
}
fn external_identity_with_hidden(path: &Path, allow_hidden_skill: bool) -> Result<Option<String>> {
    fn visit(path: &Path, h: &mut Sha256, depth: usize, allow_hidden_skill: bool) -> Result<()> {
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
            let raw_rows = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            let rows = remap_hidden_skill_entries(raw_rows, depth == 0 && allow_hidden_skill)?;
            for (logical, row) in rows {
                h.update(logical.as_bytes());
                h.update(b"\0");
                visit(&row.path(), h, depth + 1, false)?;
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
    visit(path, &mut h, 0, allow_hidden_skill)?;
    Ok(Some(format!("{:x}", h.finalize())))
}
fn external_copy(source: &Path, target: &Path, rename_top: Option<(&str, &str)>) -> Result<bool> {
    let m = fs::symlink_metadata(source)?;
    let mut renamed = false;
    if m.file_type().is_symlink() {
        symlink(fs::read_link(source)?, target)?;
    } else if m.is_file() {
        r::new_file(target, &external_read(source)?, m.mode() & 0o777)?;
    } else if m.is_dir() {
        let active_rename = rename_top.filter(|(_, to)| !r::exists(&source.join(to)));
        fs::create_dir(target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(0o700))?;
        for row in fs::read_dir(source)? {
            let row = row?;
            let name = row.file_name();
            let dest = if let Some((from, to)) = active_rename
                && name == from
                && !fs::symlink_metadata(row.path())?.is_dir()
            {
                renamed = true;
                target.join(to)
            } else {
                target.join(&name)
            };
            external_copy(&row.path(), &dest, None)?;
        }
        fs::set_permissions(target, fs::Permissions::from_mode(m.mode() & 0o777))?;
        r::sync_dir(target)?;
    } else {
        return r::fail("provider snapshot refuses special files");
    }
    Ok(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(name: &str) -> (Dir, Paths) {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("oms-tx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let paths = Paths {
            home: root.join("home"),
            source: root.join("source"),
            receipt: root.join("receipt"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cwd: root.clone(),
        };
        (Dir(root), paths)
    }

    fn write_skill_dir(dir: &Path, body: &str) {
        fs::create_dir_all(dir.join("references")).unwrap();
        // Include entries sorting before and after SKILL.md to catch sort-order drift.
        fs::write(dir.join("README.md"), b"readme\n").unwrap();
        fs::write(dir.join("SKILL.md"), body.as_bytes()).unwrap();
        fs::write(dir.join("references/note.md"), b"note\n").unwrap();
    }

    fn siblings_with_skill_md(parent: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in fs::read_dir(parent).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".oms-") && entry.path().join("SKILL.md").exists() {
                found.push(name);
            }
        }
        found
    }

    #[test]
    fn skill_directory_replace_hides_skill_md_in_stage_and_undo_and_recovers() {
        let (root, paths) = fixture("skill-hide-commit-rollback");
        let skills = root.0.join("skills");
        let target = skills.join("oms-demo");
        let source_v2 = root.0.join("source-v2");
        write_skill_dir(&target, "---\nname: oms-demo\n---\nv1\n");
        write_skill_dir(&source_v2, "---\nname: oms-demo\n---\nv2\n");
        let old_id = identity(&target).unwrap();
        let new_id = identity(&source_v2).unwrap();

        // Replace and inspect staged and undo paths before commit, then rollback on drop.
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.replace(&target, |staged| {
                let hid = copy(&source_v2, staged, Some((SKILL_FILE, HIDDEN_SKILL_FILE)))?;
                assert!(hid);
                assert!(
                    !staged.join(SKILL_FILE).exists(),
                    "staged directory must not expose top-level SKILL.md"
                );
                assert!(staged.join(HIDDEN_SKILL_FILE).is_file());
                assert_eq!(identity_with_hidden(staged, true).unwrap(), new_id);
                Ok(hid)
            })
            .unwrap();
            let change = tx.journal.changes.last().unwrap();
            assert!(change.undo.is_dir());
            assert!(
                !change.undo.join(SKILL_FILE).exists(),
                "undo directory must not expose top-level SKILL.md"
            );
            assert!(change.undo.join(HIDDEN_SKILL_FILE).is_file());
            assert!(siblings_with_skill_md(&skills).is_empty());
            assert_eq!(
                fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
                "---\nname: oms-demo\n---\nv2\n"
            );
            assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        }
        // Dropped without commit: original target with SKILL.md and old identity restored.
        assert_eq!(
            fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nv1\n"
        );
        assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        assert_eq!(identity(&target).unwrap(), old_id);
        assert!(siblings_with_skill_md(&skills).is_empty());

        // Replace and commit: target has v2 SKILL.md and undo is cleaned up.
        let mut tx = Transaction::begin(&paths).unwrap();
        tx.copy(&target, &source_v2).unwrap();
        let undo_path = tx.journal.changes.last().unwrap().undo.clone();
        assert!(undo_path.is_dir());
        assert!(!undo_path.join(SKILL_FILE).exists());
        assert!(undo_path.join(HIDDEN_SKILL_FILE).is_file());
        tx.commit().unwrap();
        assert!(!undo_path.exists());
        assert_eq!(
            fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nv2\n"
        );
        assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        assert_eq!(identity(&target).unwrap(), new_id);

        // tx.files also hides SKILL.md while staged/undo and exposes it on live target.
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.files(
                &target,
                &[
                    ("README.md", b"readme\n", 0o600),
                    ("SKILL.md", b"---\nname: oms-demo\n---\nv3\n", 0o600),
                ],
            )
            .unwrap();
            let change = tx.journal.changes.last().unwrap();
            assert!(!change.undo.join(SKILL_FILE).exists());
            assert!(change.undo.join(HIDDEN_SKILL_FILE).is_file());
            assert_eq!(
                fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
                "---\nname: oms-demo\n---\nv3\n"
            );
            assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        }
        assert_eq!(identity(&target).unwrap(), new_id);

        // Recovery from a legacy journal whose undo still holds a plain SKILL.md.
        let legacy_undo = skills.join(".oms-undo-legacy.1");
        let legacy_staged = skills.join(".oms-stage-legacy.1");
        write_skill_dir(&legacy_undo, "---\nname: oms-demo\n---\nv1\n");
        let journal_dir = paths.config.join("native-lifecycle");
        fs::create_dir_all(&journal_dir).unwrap();
        let journal = json!({
            "schema": 1,
            "committed": false,
            "changes": [{
                "target": target,
                "staged": legacy_staged,
                "undo": legacy_undo,
                "old": old_id,
                "new": new_id,
                "external": false,
                "parent_identity": null
            }],
            "effects": []
        });
        fs::write(
            journal_dir.join("transaction.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let tx = Transaction::begin(&paths).unwrap();
        tx.commit().unwrap();
        assert!(!legacy_undo.exists());
        assert_eq!(
            fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nv1\n"
        );
        assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        assert_eq!(identity(&target).unwrap(), old_id);
    }

    #[test]
    fn symlink_skill_rollback_and_recovery_do_not_expose_undo() {
        let (root, paths) = fixture("symlink-skill-recovery");
        let skills = root.0.join("skills");
        fs::create_dir_all(&skills).unwrap();
        let old = root.0.join("old");
        let new = root.0.join("new");
        write_skill_dir(&old, "old");
        write_skill_dir(&new, "new");
        let target = skills.join("oms-demo");
        let old_text = PathBuf::from("../old");
        symlink(&old_text, &target).unwrap();

        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.link(&target, &new).unwrap();
            assert!(siblings_with_skill_md(&skills).is_empty());
            assert_eq!(fs::read_link(&target).unwrap(), new);
        }
        assert_eq!(fs::read_link(&target).unwrap(), old_text);
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.remove(&target).unwrap();
            assert!(siblings_with_skill_md(&skills).is_empty());
            assert!(!r::exists(&target));
            // Leave the saved journal in place to simulate interruption before commit.
            tx.complete = true;
        }
        Transaction::begin(&paths).unwrap().commit().unwrap();
        assert_eq!(fs::read_link(&target).unwrap(), old_text);
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.link(&target, &new).unwrap();
            tx.complete = true;
        }
        Transaction::begin(&paths).unwrap().commit().unwrap();
        assert_eq!(fs::read_link(&target).unwrap(), old_text);
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            let start = tx.prepare_external(std::slice::from_ref(&target)).unwrap();
            assert!(siblings_with_skill_md(&skills).is_empty());
            fs::remove_file(&target).unwrap();
            symlink(&new, &target).unwrap();
            tx.finish_external(start).unwrap();
        }
        assert_eq!(fs::read_link(&target).unwrap(), old_text);
        assert!(siblings_with_skill_md(&skills).is_empty());
    }

    #[test]
    fn skill_directory_crash_windows_external_remove_and_symlinks() {
        let (root, paths) = fixture("skill-crash-external-symlink");
        let skills = root.0.join("skills");
        let target = skills.join("oms-demo");
        let source_v2 = root.0.join("source-v2");
        write_skill_dir(&target, "---\nname: oms-demo\n---\nv1\n");
        write_skill_dir(&source_v2, "---\nname: oms-demo\n---\nv2\n");
        let old_id = identity(&target).unwrap();
        let new_id = identity(&source_v2).unwrap();

        // Crash between `staged -> target` rename and `unhide_skill(target)`:
        // target still holds hidden v2, undo holds hidden v1; recovery restores plain v1.
        let crash_undo = skills.join(".oms-undo-crash.1");
        let crash_staged = skills.join(".oms-stage-crash.1");
        fs::rename(&target, &crash_undo).unwrap();
        hide_skill(&crash_undo).unwrap();
        assert!(copy(&source_v2, &target, Some((SKILL_FILE, HIDDEN_SKILL_FILE))).unwrap());
        assert!(!target.join(SKILL_FILE).exists());
        assert!(target.join(HIDDEN_SKILL_FILE).is_file());
        let journal_dir = paths.config.join("native-lifecycle");
        fs::create_dir_all(&journal_dir).unwrap();
        let journal = json!({
            "schema": 1,
            "committed": false,
            "changes": [{
                "target": target,
                "staged": crash_staged,
                "undo": crash_undo,
                "old": old_id,
                "new": new_id,
                "external": false,
                "parent_identity": null,
                "hidden_old_skill": true,
                "hidden_new_skill": true
            }],
            "effects": []
        });
        fs::write(
            journal_dir.join("transaction.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let tx = Transaction::begin(&paths).unwrap();
        tx.commit().unwrap();
        assert!(!crash_undo.exists());
        assert_eq!(
            fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nv1\n"
        );
        assert!(!target.join(HIDDEN_SKILL_FILE).exists());
        assert_eq!(identity(&target).unwrap(), old_id);

        // Crash after `save()` before `rename(target, undo)`: staged holds hidden v2.
        let pre_undo = skills.join(".oms-undo-pre.2");
        let pre_staged = skills.join(".oms-stage-pre.2");
        assert!(
            copy(
                &source_v2,
                &pre_staged,
                Some((SKILL_FILE, HIDDEN_SKILL_FILE))
            )
            .unwrap()
        );
        let journal = json!({
            "schema": 1,
            "committed": false,
            "changes": [{
                "target": target,
                "staged": pre_staged,
                "undo": pre_undo,
                "old": old_id,
                "new": new_id,
                "external": false,
                "parent_identity": null,
                "hidden_old_skill": true,
                "hidden_new_skill": true
            }],
            "effects": []
        });
        fs::write(
            journal_dir.join("transaction.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let tx = Transaction::begin(&paths).unwrap();
        tx.commit().unwrap();
        assert!(!pre_staged.exists());
        assert_eq!(identity(&target).unwrap(), old_id);

        // tx.remove hides undo SKILL.md before commit and restores on rollback.
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.remove(&target).unwrap();
            let change = tx.journal.changes.last().unwrap();
            assert!(!target.exists());
            assert!(!change.undo.join(SKILL_FILE).exists());
            assert!(change.undo.join(HIDDEN_SKILL_FILE).is_file());
        }
        assert_eq!(identity(&target).unwrap(), old_id);
        assert!(target.join(SKILL_FILE).is_file());

        // prepare_external snapshots with hidden SKILL.md and restores cleanly on rollback.
        let old_ext_id = external_identity(&target).unwrap();
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            let start = tx.prepare_external(std::slice::from_ref(&target)).unwrap();
            let undo_path = tx.journal.changes.last().unwrap().undo.clone();
            assert!(!undo_path.join(SKILL_FILE).exists());
            assert!(undo_path.join(HIDDEN_SKILL_FILE).is_file());
            fs::write(target.join(SKILL_FILE), b"---\nname: oms-demo\n---\next\n").unwrap();
            tx.finish_external(start).unwrap();
        }
        assert_eq!(external_identity(&target).unwrap(), old_ext_id);
        assert_eq!(
            fs::read_to_string(target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nv1\n"
        );

        // Symlink to a skill directory must never rename the symlink target's SKILL.md.
        let link_target = skills.join("oms-linked");
        symlink(&source_v2, &link_target).unwrap();
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.copy(&link_target, &target).unwrap();
            assert!(
                source_v2.join(SKILL_FILE).is_file(),
                "undo of a symlink must never mutate the referenced source directory"
            );
            assert!(!source_v2.join(HIDDEN_SKILL_FILE).exists());
        }
        assert!(
            fs::symlink_metadata(&link_target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(source_v2.join(SKILL_FILE).is_file());

        // A directory that legitimately contains only `.oms-hidden-SKILL.md` is not corrupted.
        let odd_src = root.0.join("odd-src");
        fs::create_dir_all(&odd_src).unwrap();
        fs::write(odd_src.join(HIDDEN_SKILL_FILE), b"literal\n").unwrap();
        let odd_target = skills.join("oms-odd");
        let odd_id = identity(&odd_src).unwrap();
        let mut tx = Transaction::begin(&paths).unwrap();
        tx.copy(&odd_target, &odd_src).unwrap();
        tx.commit().unwrap();
        assert!(!odd_target.join(SKILL_FILE).exists());
        assert_eq!(
            fs::read_to_string(odd_target.join(HIDDEN_SKILL_FILE)).unwrap(),
            "literal\n"
        );
        assert_eq!(identity(&odd_target).unwrap(), odd_id);

        // Read-only sticky directory round-trips full permission bits through stage and rollback.
        let ro_target = skills.join("oms-ro-sticky");
        write_skill_dir(&ro_target, "---\nname: oms-ro\n---\nv1\n");
        fs::set_permissions(&ro_target, fs::Permissions::from_mode(0o1500)).unwrap();
        struct RestoreWritable(PathBuf);
        impl Drop for RestoreWritable {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            }
        }
        let _ro_guard = RestoreWritable(ro_target.clone());
        assert_eq!(
            fs::symlink_metadata(&ro_target).unwrap().mode() & 0o7777,
            0o1500
        );
        let ro_old_ext_id = external_identity(&ro_target).unwrap();
        {
            let mut tx = Transaction::begin(&paths).unwrap();
            tx.publish_external(&ro_target, &source_v2).unwrap();
            let change = tx.journal.changes.last().unwrap();
            assert!(!change.undo.join(SKILL_FILE).exists());
            assert!(change.undo.join(HIDDEN_SKILL_FILE).is_file());
            assert_eq!(
                fs::symlink_metadata(&change.undo).unwrap().mode() & 0o7777,
                0o1500,
                "hiding SKILL.md on a read-only sticky undo dir must preserve special mode bits"
            );
        }
        assert_eq!(
            fs::symlink_metadata(&ro_target).unwrap().mode() & 0o7777,
            0o1500
        );
        assert_eq!(external_identity(&ro_target).unwrap(), ro_old_ext_id);
        assert_eq!(
            fs::read_to_string(ro_target.join(SKILL_FILE)).unwrap(),
            "---\nname: oms-ro\n---\nv1\n"
        );
        assert!(!ro_target.join(HIDDEN_SKILL_FILE).exists());

        // A foreign SKILL.md present at the unhide destination is refused, not overwritten.
        let foreign_dir = skills.join("oms-foreign-unhide");
        fs::create_dir_all(&foreign_dir).unwrap();
        fs::write(
            foreign_dir.join(HIDDEN_SKILL_FILE),
            b"---\nname: oms-demo\n---\nhidden\n",
        )
        .unwrap();
        fs::write(foreign_dir.join(SKILL_FILE), b"foreign-content\n").unwrap();
        fs::set_permissions(&foreign_dir, fs::Permissions::from_mode(0o1500)).unwrap();
        let _foreign_guard = RestoreWritable(foreign_dir.clone());
        let err = unhide_skill(&foreign_dir).unwrap_err();
        assert!(
            err.1.contains("already exists"),
            "unexpected error: {}",
            err.1
        );
        assert_eq!(
            fs::read_to_string(foreign_dir.join(SKILL_FILE)).unwrap(),
            "foreign-content\n"
        );
        assert_eq!(
            fs::read_to_string(foreign_dir.join(HIDDEN_SKILL_FILE)).unwrap(),
            "---\nname: oms-demo\n---\nhidden\n"
        );
        assert_eq!(
            fs::symlink_metadata(&foreign_dir).unwrap().mode() & 0o7777,
            0o1500
        );
    }
}
