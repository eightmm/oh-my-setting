use crate::{Error, Result, Value, decode, identifier, require};
use oms_lock::{Guard, Lock, Mode, Settings};
use oms_safe_fs::{ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const MAX_STATE: usize = 8 * 1024 * 1024;
/// Read caller-supplied scratch input through one descriptor, including external
/// files. NONBLOCK avoids hanging on a FIFO before the regular-file check.
pub(crate) fn input(repo: &Path, path: &Path, maximum: usize) -> Result<Vec<u8>> {
    use rustix::fs::{self, FileType, Mode, OFlags};
    use std::io::Read;
    let target = if path.is_absolute() {
        path.to_owned()
    } else {
        repo.join(path)
    };
    let fd = fs::open(
        &target,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(Error::new)?;
    let before = fs::fstat(&fd).map_err(Error::new)?;
    require(
        FileType::from_raw_mode(before.st_mode) == FileType::RegularFile
            && before.st_nlink == 1
            && before.st_uid == rustix::process::getuid().as_raw()
            && before.st_size >= 0
            && before.st_size as u64 <= maximum as u64,
        "input must be an owned single-link bounded regular file",
    )?;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::new)?;
    let after = fs::fstat(&file).map_err(Error::new)?;
    require(
        before.st_dev == after.st_dev
            && before.st_ino == after.st_ino
            && before.st_size == after.st_size
            && before.st_mtime == after.st_mtime
            && before.st_mtime_nsec == after.st_mtime_nsec
            && before.st_ctime == after.st_ctime
            && before.st_ctime_nsec == after.st_ctime_nsec
            && after.st_nlink == 1
            && bytes.len() as u64 == before.st_size as u64
            && bytes.len() <= maximum,
        "input changed while reading",
    )?;
    require(!bytes.contains(&0), "input contains NUL")?;
    crate::outbound(&bytes)?;
    Ok(bytes)
}
/// A captured physical root. No creation or repair occurs at open.
pub struct Store {
    root: Root,
    settings: Settings,
}
pub struct Transaction<'a> {
    store: &'a Store,
    guard: Guard,
}
impl Store {
    pub fn open(repo: &Path) -> Result<Self> {
        Self::with_settings(repo, Settings::from_env().map_err(Error::new)?)
    }
    pub fn with_settings(repo: &Path, settings: Settings) -> Result<Self> {
        Ok(Self {
            root: Root::open(repo).map_err(|e| Error::new(format!("panel root: {e:?}")))?,
            settings,
        })
    }
    pub fn repo(&self) -> &Path {
        self.root.physical_root()
    }
    pub fn read(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        path(relative)?;
        self.root
            .read_no_follow(
                relative,
                ByteReadOptions {
                    max_bytes: maximum,
                    missing_ok: missing,
                },
            )
            .map_err(|e| Error::new(format!("panel read refused: {e:?}")))
    }
    pub fn json(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Option<Value>> {
        let bytes = self.read(relative, maximum, missing)?;
        if missing && bytes.is_empty() {
            Ok(None)
        } else {
            decode(&bytes, maximum).map(Some)
        }
    }
    /// Direct flock shares the Python panel lock, independent of the shell lock selector.
    pub fn transaction(&self, name: &str, timeout: Duration) -> Result<Transaction<'_>> {
        identifier(name)?;
        require(!timeout.is_zero(), "panel lock timeout must be positive")?;
        self.lock_at(Path::new(&format!(".oms/artifacts/{name}.lock")), timeout)
    }
    pub fn lock_at(&self, relative: &Path, timeout: Duration) -> Result<Transaction<'_>> {
        path(relative)?;
        require(!timeout.is_zero(), "panel lock timeout must be positive")?;
        self.lock_at_raw(relative, timeout).map_err(|e| Error {
            code: e.exit_code(),
            ..Error::new(e)
        })
    }
    pub(crate) fn lock_at_raw(
        &self,
        relative: &Path,
        timeout: Duration,
    ) -> std::result::Result<Transaction<'_>, oms_lock::Error> {
        path(relative).map_err(|_| oms_lock::Error::InvalidTarget)?;
        if timeout.is_zero() {
            return Err(oms_lock::Error::InvalidSettings);
        }
        let mut settings = self.settings.clone();
        settings.mode = Mode::Flock;
        settings.timeout = timeout;
        let guard = Lock::in_root(self.repo().to_path_buf(), relative, settings)?.acquire(None)?;
        Ok(Transaction { store: self, guard })
    }
}
pub(crate) fn path(relative: &Path) -> Result<()> {
    require(
        !relative.as_os_str().is_empty()
            && !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "panel path must be repository-relative",
    )
}
impl Transaction<'_> {
    pub fn check(&self) -> Result<()> {
        self.check_raw().map_err(|e| Error {
            code: e.exit_code(),
            ..Error::new(e)
        })
    }
    pub(crate) fn check_raw(&self) -> std::result::Result<(), oms_lock::Error> {
        self.guard.check()
    }
    pub fn read(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        self.check()?;
        self.store.read(relative, maximum, missing)
    }
    pub fn json(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Option<Value>> {
        self.check()?;
        self.store.json(relative, maximum, missing)
    }
    pub fn mutate(
        &self,
        relative: &Path,
        maximum: usize,
        transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        path(relative)?;
        self.check()?;
        self.store
            .root
            .atomic_mutate(
                relative,
                MutationOptions {
                    read: ByteReadOptions {
                        missing_ok: true,
                        max_bytes: maximum,
                    },
                    create_parent: true,
                    max_output: maximum,
                    ..MutationOptions::default()
                },
                transform,
            )
            .map(|v| v.bytes)
            .map_err(|e| match e {
                MutationError::Transform(e) => e,
                MutationError::Storage { phase, error } => Error {
                    publication_uncertain: phase != MutationPhase::BeforePublication,
                    ..Error::new(format!(
                        "panel mutation {phase:?}: {error:?}; reconcile before retry"
                    ))
                },
            })
    }
    pub fn write_json(
        &self,
        relative: &Path,
        value: &Value,
        immutable: bool,
        maximum: usize,
    ) -> Result<PathBuf> {
        let body = crate::value::encoded(value)?;
        self.write(relative, &body, immutable, maximum)?;
        Ok(self.store.repo().join(relative))
    }
    pub fn write(
        &self,
        relative: &Path,
        body: &[u8],
        immutable: bool,
        maximum: usize,
    ) -> Result<()> {
        require(body.len() <= maximum, "panel artifact exceeds byte limit")?;
        self.mutate(relative, maximum, |old| {
            require(
                !immutable || old.is_empty() || old == body,
                "immutable panel artifact conflicts with existing bytes",
            )?;
            Ok(body.to_vec())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn identical_json_write_preserves_file_identity() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "oms-panel-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let tx = store
            .transaction("snapshot-test", Duration::from_secs(2))
            .unwrap();
        let path = Path::new(".oms/panel-state/snapshot.json");
        let value = serde_json::json!({"status": "ready"});
        tx.write_json(path, &value, false, MAX_STATE).unwrap();
        let before = std::fs::metadata(root.join(path)).unwrap();
        tx.write_json(path, &value, false, MAX_STATE).unwrap();
        let after = std::fs::metadata(root.join(path)).unwrap();
        assert_eq!(
            (before.ino(), before.mtime(), before.mtime_nsec()),
            (after.ino(), after.mtime(), after.mtime_nsec())
        );
        drop(tx);
        std::fs::remove_dir_all(root).unwrap();
    }
}
