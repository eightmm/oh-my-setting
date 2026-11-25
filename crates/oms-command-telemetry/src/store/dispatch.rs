//! The existing panel reservation namespace and exact flock authority, shared
//! with physical-call admission. Payload publication and registry CAS are
//! separate effects; callers must reconcile a partial publication before retry.
use super::{StoreError, cancelled, resources};
use oms_json::{ParseOptions, Value};
use oms_lock::{Guard, Lock, Mode, Settings};
use oms_safe_fs::{
    ByteError, ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const REGISTRY: &str = ".oms/artifacts/panel-dispatch/reservations.json";
pub const LOCK: &str = ".oms/artifacts/panel-dispatch.lock";
pub const MAX_REGISTRY: usize = 4 * 1024 * 1024;
pub const MAX_ARTIFACT: usize = 64 * 1024;

#[derive(Debug)]
pub struct Error {
    pub source: StoreError,
    /// A preceding payload write or this failing mutation may already be visible.
    pub publication_uncertain: bool,
    /// Repository-relative file whose step failed, when one is attributable.
    pub(super) file: Option<&'static str>,
}
impl Error {
    pub(super) fn new(source: StoreError, publication_uncertain: bool) -> Self {
        Self {
            source,
            publication_uncertain,
            file: None,
        }
    }
    pub fn exit_code(&self) -> i32 {
        self.source.exit_code()
    }
    pub fn file(&self) -> Option<&'static str> {
        self.file
    }
    /// The first attribution wins: a caller naming the step it ran must not
    /// relabel a lost dispatch lock.
    pub(super) fn in_file(mut self, file: &'static str) -> Self {
        self.file.get_or_insert(file);
        self
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.file {
            Some(file) => write!(f, "dispatch reservation: {file}: {}", self.source),
            None => write!(f, "dispatch reservation: {}", self.source),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

pub struct Transaction<'a> {
    root: Root,
    guard: Guard,
    interrupted: &'a AtomicBool,
    published: Cell<bool>,
    lost: Cell<bool>,
}
impl<'a> Transaction<'a> {
    pub fn acquire(repo: &Path, timeout: Duration, interrupted: &'a AtomicBool) -> Result<Self> {
        let settings =
            Settings::from_env().map_err(|error| Error::new(StoreError::Lock(error), false))?;
        Self::with_settings(repo, settings, timeout, interrupted)
    }
    pub fn with_settings(
        repo: &Path,
        mut settings: Settings,
        timeout: Duration,
        interrupted: &'a AtomicBool,
    ) -> Result<Self> {
        let prepare = || -> std::result::Result<(Root, Guard), StoreError> {
            cancelled(interrupted)?;
            if timeout.is_zero() {
                return Err(StoreError::Invalid(
                    "dispatch lock timeout must be positive",
                ));
            }
            let root = Root::open(repo)?;
            settings.mode = Mode::Flock;
            settings.timeout = timeout;
            let lock = Lock::in_root(root.physical_root().to_owned(), Path::new(LOCK), settings)
                .map_err(StoreError::Lock)?;
            let start = Instant::now();
            let guard = loop {
                cancelled(interrupted)?;
                match lock.try_acquire() {
                    Ok(guard) => break guard,
                    Err(oms_lock::Error::Busy) if start.elapsed() < timeout => std::thread::sleep(
                        Duration::from_millis(20).min(timeout.saturating_sub(start.elapsed())),
                    ),
                    Err(error) => return Err(StoreError::Lock(error)),
                }
            };
            Ok((root, guard))
        };
        let (root, guard) = prepare().map_err(|source| Error::new(source, false))?;
        Ok(Self {
            root,
            guard,
            interrupted,
            published: Cell::new(false),
            lost: Cell::new(false),
        })
    }
    pub fn repo(&self) -> &Path {
        self.root.physical_root()
    }
    pub(super) fn error(&self, source: StoreError) -> Error {
        let visible = matches!(&source, StoreError::Storage { phase, .. } if *phase != oms_safe_fs::MutationPhase::BeforePublication);
        if visible {
            self.published.set(true);
        }
        let error = Error::new(source, self.published.get() || visible);
        if self.lost.get() {
            error.in_file(LOCK)
        } else {
            error
        }
    }
    pub(super) fn check(&self) -> std::result::Result<(), StoreError> {
        cancelled(self.interrupted)?;
        self.guard
            .check()
            .inspect_err(|_| self.lost.set(true))
            .map_err(StoreError::Lock)
    }
    fn validate_path(&self, relative: &Path) -> std::result::Result<(), StoreError> {
        super::paths::validate(&self.root, &self.repo().join(relative), false)
    }
    /// Exact original bytes are the compare-and-swap token. No reserialization
    /// or typed projection may silently discard another producer's fields.
    pub fn snapshot(&self) -> Result<Option<Vec<u8>>> {
        let read = || -> std::result::Result<Option<Vec<u8>>, StoreError> {
            self.check()?;
            let bytes = snapshot(&self.root, self.interrupted, false)?;
            self.check()?;
            Ok(bytes.map(|state| state.raw))
        };
        read().map_err(|source| self.error(source).in_file(REGISTRY))
    }
    pub fn replace(&self, expected: Option<&[u8]>, candidate: &[u8]) -> Result<()> {
        validate_registry(candidate, false).map_err(|source| self.error(source))?;
        self.mutate(
            Path::new(REGISTRY),
            MAX_REGISTRY,
            expected.is_some(),
            |old| {
                if old != expected.unwrap_or_default() {
                    return Err(StoreError::Changed);
                }
                if expected.is_some() {
                    validate_registry(old, false)?;
                }
                Ok(candidate.to_vec())
            },
        )
        .map_err(|error| error.in_file(REGISTRY))
    }
    /// Immutable launch/process evidence retains the legacy public schema.
    pub fn artifact(&self, member: &str, phase: &str, body: &[u8]) -> Result<PathBuf> {
        let validate = || -> std::result::Result<PathBuf, StoreError> {
            if member.is_empty()
                || member.len() > 160
                || !member
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
                || !matches!(phase, "launch" | "process")
            {
                return Err(StoreError::Invalid("invalid dispatch artifact identity"));
            }
            if body.len() > MAX_ARTIFACT {
                return Err(StoreError::Invalid("dispatch artifact exceeds byte limit"));
            }
            let value = decode(body, MAX_ARTIFACT)?;
            let schema = if phase == "launch" { "2" } else { "1" };
            if !value
                .get("member")
                .and_then(Value::as_string)
                .is_some_and(|value| value == &oms_json::PyString::from(member))
                || !has_schema(&value, schema)
            {
                return Err(StoreError::Invalid("dispatch artifact identity mismatch"));
            }
            Ok(PathBuf::from(format!(
                ".oms/artifacts/panel-dispatch/{member}-{phase}.json"
            )))
        };
        let relative = validate().map_err(|source| self.error(source))?;
        self.check().map_err(|source| self.error(source))?;
        self.validate_path(&relative)
            .map_err(|source| self.error(source))?;
        let captured = match self.root.read_no_follow(
            &relative,
            ByteReadOptions {
                missing_ok: false,
                max_bytes: MAX_ARTIFACT,
            },
        ) {
            Ok(bytes) => Some(bytes),
            Err(ByteError::Missing) => None,
            Err(error) => return Err(self.error(error.into())),
        };
        if captured.as_deref().is_some_and(|old| old != body) {
            return Err(self.error(StoreError::Invalid(
                "immutable dispatch artifact conflicts with existing bytes",
            )));
        }
        self.mutate(&relative, MAX_ARTIFACT, captured.is_some(), |old| {
            if old != captured.as_deref().unwrap_or_default() {
                return Err(StoreError::Invalid(
                    "immutable dispatch artifact conflicts with existing bytes",
                ));
            }
            Ok(body.to_vec())
        })?;
        Ok(self.repo().join(relative))
    }
    fn mutate(
        &self,
        relative: &Path,
        maximum: usize,
        expected_exists: bool,
        transform: impl FnOnce(&[u8]) -> std::result::Result<Vec<u8>, StoreError>,
    ) -> Result<()> {
        let result = (|| -> std::result::Result<(), StoreError> {
            self.check()?;
            self.validate_path(relative)?;
            self.root
                .atomic_mutate(
                    relative,
                    MutationOptions {
                        read: ByteReadOptions {
                            missing_ok: true,
                            max_bytes: maximum,
                        },
                        create_parent: true,
                        max_output: maximum,
                        expected_exists: Some(expected_exists),
                        ..MutationOptions::default()
                    },
                    |old| {
                        self.check()?;
                        self.validate_path(relative)?;
                        let bytes = transform(old)?;
                        self.check()?;
                        Ok(bytes)
                    },
                )
                .map_err(|error| match error {
                    MutationError::Storage {
                        phase: MutationPhase::BeforePublication,
                        error: ByteError::ExistenceMismatch,
                    } => StoreError::Changed,
                    error => error.into(),
                })?;
            self.published.set(true);
            self.validate_path(relative)?;
            self.check()
        })();
        result.map_err(|source| self.error(source))
    }
}
/// Read-only observation, without creating a lock or granting admission. A
/// writer must acquire Transaction and recheck its own snapshot before a CAS.
pub fn read_snapshot(
    repo: &Path,
    interrupted: &AtomicBool,
) -> std::result::Result<Option<Vec<u8>>, StoreError> {
    Ok(snapshot(&Root::open(repo)?, interrupted, false)?.map(|state| state.raw))
}
pub fn validate_registry_bytes(body: &[u8]) -> std::result::Result<(), StoreError> {
    validate_registry(body, false).map(|_| ())
}
pub(super) struct RegistrySnapshot {
    pub raw: Vec<u8>,
    pub resources: Option<resources::Summary>,
}
pub(super) fn read_resource_snapshot(
    repo: &Path,
    interrupted: &AtomicBool,
) -> std::result::Result<Option<RegistrySnapshot>, StoreError> {
    snapshot(&Root::open(repo)?, interrupted, true)
}
fn snapshot(
    root: &Root,
    interrupted: &AtomicBool,
    inspect_legacy_resources: bool,
) -> std::result::Result<Option<RegistrySnapshot>, StoreError> {
    cancelled(interrupted)?;
    let path = root.physical_root().join(REGISTRY);
    super::paths::validate(root, &path, false)?;
    let bytes = match root.read_no_follow(
        Path::new(REGISTRY),
        ByteReadOptions {
            missing_ok: false,
            max_bytes: MAX_REGISTRY,
        },
    ) {
        Ok(bytes) => bytes,
        Err(ByteError::Missing) => {
            cancelled(interrupted)?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    cancelled(interrupted)?;
    super::paths::validate(root, &path, false)?;
    let resources = validate_registry(&bytes, inspect_legacy_resources)?;
    Ok(Some(RegistrySnapshot {
        raw: bytes,
        resources,
    }))
}
fn validate_registry(
    body: &[u8],
    inspect_legacy_resources: bool,
) -> std::result::Result<Option<resources::Summary>, StoreError> {
    if body.len() > MAX_REGISTRY {
        return Err(StoreError::Invalid("dispatch registry exceeds byte limit"));
    }
    let value = decode(body, MAX_REGISTRY)?;
    if !(has_schema(&value, "1") || has_schema(&value, "2"))
        || value.get("calls").and_then(Value::as_object).is_none()
    {
        return Err(StoreError::Invalid(
            "unsupported dispatch registry schema or calls",
        ));
    }
    let has_resources = has_schema(&value, "2") || resources::has_contract(&value);
    if has_resources || inspect_legacy_resources {
        let registry = crate::value::parse(body, true).map_err(|_| {
            StoreError::Invalid(if has_resources {
                "invalid resource registry"
            } else {
                "invalid resource JSON"
            })
        })?;
        return resources::validate(&registry);
    }
    Ok(None)
}
fn has_schema(value: &Value, schema: &str) -> bool {
    value
        .get("schema")
        .and_then(Value::as_integer)
        .is_some_and(|value| value.as_decimal() == schema)
}
fn decode(body: &[u8], maximum: usize) -> std::result::Result<Value, StoreError> {
    oms_json::parse_with_options(
        body,
        &ParseOptions {
            max_bytes: maximum,
            max_depth: 128,
            ..ParseOptions::strict()
        },
    )
    .map_err(|_| StoreError::Invalid("malformed dispatch JSON"))
}
