use crate::{Error, Result, Value, decode, identifier, require};
use oms_lock::{Guard, Lock, Mode, Settings};
use oms_safe_fs::{ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const MAX_STATE: usize = 8 * 1024 * 1024;
/// A captured physical root. No creation or repair occurs at open.
pub struct Store {
    root: Root,
    settings: Settings,
}
pub struct Transaction<'a> {
    store: &'a Store,
    _guard: Guard,
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
        let lock_path = self.repo().join(relative);
        let mut settings = self.settings.clone();
        settings.mode = Mode::Flock;
        settings.timeout = timeout;
        let guard = Lock::at_path(lock_path, settings)
            .map_err(Error::new)?
            .acquire(None)
            .map_err(|e| Error {
                code: e.exit_code(),
                ..Error::new(e)
            })?;
        Ok(Transaction {
            store: self,
            _guard: guard,
        })
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
    pub fn read(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        self.store.read(relative, maximum, missing)
    }
    pub fn json(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Option<Value>> {
        self.store.json(relative, maximum, missing)
    }
    pub fn mutate(
        &self,
        relative: &Path,
        maximum: usize,
        transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        path(relative)?;
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
