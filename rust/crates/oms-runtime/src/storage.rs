//! Command-local byte snapshots and canonical locked durable writes.
use crate::value::*;
use crate::{Result, Services, error, require};
use oms_command_plan::storage::Store;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
pub const MAX_STATE: usize = 8 * 1024 * 1024;
pub const MAX_STREAM: usize = 32 * 1024 * 1024;
pub const MAX_ROW: usize = 1024 * 1024;
pub const MAX_ROWS: usize = 250_000;
pub struct Runtime<'a> {
    pub repo: PathBuf,
    pub cwd: PathBuf,
    pub state_repo: PathBuf,
    pub install_root: PathBuf,
    pub services: &'a dyn Services,
    pub interrupted: &'a AtomicBool,
    pub now: String,
    pub repository: crate::services::RepositoryView,
    pub(crate) store: Store,
    pub(crate) sources: Store,
    cache: RefCell<BTreeMap<PathBuf, Option<Arc<Vec<u8>>>>>,
    generations: RefCell<BTreeMap<PathBuf, Option<Value>>>,
}
impl<'a> Runtime<'a> {
    /// Logical and physical execution/state roots are distinct. The adapter resolves
    /// Git's common state root; a runtime projection never chooses it from a capsule.
    pub fn open(
        repo: &Path,
        state_repo: &Path,
        install_root: &Path,
        services: &'a dyn Services,
        interrupted: &'a AtomicBool,
    ) -> Result<Self> {
        let groups = services.group_resolver();
        let sources = Store::open(repo)?.with_group_resolver(groups);
        let store = Store::open(state_repo)?.with_group_resolver(groups);
        let repository = services.repository_view(sources.repo())?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?;
        let now = chrono::DateTime::from_timestamp(stamp.as_secs() as i64, 0)
            .ok_or_else(|| error("invalid clock"))?
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        Ok(Self {
            repo: sources.repo().to_path_buf(),
            cwd: std::env::current_dir().map_err(error)?,
            state_repo: store.repo().to_path_buf(),
            install_root: std::fs::canonicalize(install_root).map_err(error)?,
            services,
            interrupted,
            now,
            repository,
            store,
            sources,
            cache: RefCell::new(BTreeMap::new()),
            generations: RefCell::new(BTreeMap::new()),
        })
    }
    pub fn check(&self) -> Result<()> {
        if self.interrupted.load(Ordering::Relaxed) {
            Err(crate::status(130, "interrupted"))
        } else {
            Ok(())
        }
    }
    fn owner(&self, path: &Path) -> &Store {
        if path
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".oms")
        {
            &self.store
        } else {
            &self.sources
        }
    }
    pub fn bytes(&self, path: impl AsRef<Path>, limit: usize) -> Result<Option<Arc<Vec<u8>>>> {
        self.check()?;
        let path = path.as_ref();
        let key = self.owner(path).repo().join(path);
        if let Some(value) = self.cache.borrow().get(&key) {
            require(
                value.as_ref().is_none_or(|v| v.len() <= limit),
                "source exceeds read bound",
            )?;
            return Ok(value.clone());
        }
        let snapshot = self.owner(path).snapshot(path, limit, true)?;
        self.generations
            .borrow_mut()
            .insert(key.clone(), snapshot.as_ref().map(|s| s.generation.clone()));
        let value = snapshot.map(|s| Arc::new(s.raw));
        self.cache.borrow_mut().insert(key, value.clone());
        Ok(value)
    }
    pub fn json(&self, path: impl AsRef<Path>, default: Value) -> Result<Value> {
        match self.bytes(path, MAX_STATE)? {
            Some(raw) => parse(&raw),
            None => Ok(default),
        }
    }
    pub fn text(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
        self.bytes(path, MAX_STATE)?
            .map(|v| String::from_utf8(v.to_vec()).map_err(error))
            .transpose()
    }
    pub fn rows(&self, path: impl AsRef<Path>) -> Result<Vec<Value>> {
        match self.bytes(path, MAX_STREAM)? {
            Some(v) => jsonl(&v),
            None => Ok(vec![]),
        }
    }
    pub fn descriptor(&self, path: &str) -> Result<Value> {
        let raw = self
            .bytes(path, MAX_STATE)?
            .ok_or_else(|| error("source disappeared"))?;
        Ok(object([
            ("path", string(path)),
            ("bytes", number(raw.len())),
            ("sha256", string(sha(raw.as_slice()))),
        ]))
    }
    pub fn invalidate(&self, path: &Path) {
        let key = self.owner(path).repo().join(path);
        self.cache.borrow_mut().remove(&key);
        self.generations.borrow_mut().remove(&key);
    }
    pub fn parent_write(&self) -> Result<()> {
        require(
            std::env::var_os("OMS_HARNESS_CHILD").as_deref() != Some(std::ffi::OsStr::new("1")),
            "a harness child may only use read-only runtime actions",
        )
    }
    pub fn lock(&self, path: &Path) -> Result<oms_lock::Guard> {
        self.parent_write()?;
        self.check()?;
        require(
            !path.is_absolute()
                && path.components().all(|c| {
                    matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }),
            "internal runtime lock requires a confined relative path",
        )?;
        crate::io::lock(self, &self.owner(path).repo().join(path))
    }
    pub fn write(&self, path: &Path, raw: &[u8], limit: usize) -> Result<()> {
        let _guard = self.lock(path)?;
        self.write_locked(path, raw, limit, &_guard)
    }
    pub fn write_locked(
        &self,
        path: &Path,
        raw: &[u8],
        limit: usize,
        guard: &oms_lock::Guard,
    ) -> Result<()> {
        self.parent_write()?;
        self.check()?;
        require(
            !path.is_absolute()
                && path.components().all(|c| {
                    matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }),
            "internal runtime writes require a confined relative path",
        )?;
        require(raw.len() <= limit, "runtime output exceeds the byte limit")?;
        let root = self.owner(path).repo();
        let absolute = root.join(path);
        let generation = self.generations.borrow().get(&absolute).cloned();
        let result = crate::io::state(self, &absolute, raw, root, guard, generation);
        self.invalidate(path);
        result
    }
    /// Authority is supplied by an explicit user output flag. Default/internal
    /// destinations must continue through the confined write/write_json methods.
    pub fn write_output(&self, path: &Path, raw: &[u8], limit: usize) -> Result<()> {
        self.parent_write()?;
        self.check()?;
        require(raw.len() <= limit, "runtime output exceeds the byte limit")?;
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            self.repo.join(path)
        };
        let result = crate::io::output(self, &absolute, raw);
        self.invalidate(&absolute);
        result
    }
    pub fn write_json_output(&self, path: &Path, value: &Value) -> Result<()> {
        // Python atomic_write_json has no state read bound for explicit outputs.
        let mut raw = sorted_pretty(value)?;
        raw.push(b'\n');
        self.write_output(path, &raw, raw.len())
    }
    pub fn write_json(&self, path: &Path, value: &Value) -> Result<()> {
        let mut raw = sorted_pretty(value)?;
        raw.push(b'\n');
        self.write(path, &raw, MAX_STATE)
    }
    pub fn append(&self, path: &Path, row: &Value) -> Result<()> {
        self.parent_write()?;
        row.object()?;
        let encoded = encoded(row)?;
        require(
            encoded.len() <= MAX_ROW,
            "refusing JSONL row larger than 1 MiB",
        )?;
        let guard = self.lock(path)?;
        self.append_prepared(path, &encoded, &guard)
    }
    /// The guard must own this stream's canonical runtime-SHA256 lock.
    pub fn append_locked(&self, path: &Path, row: &Value, guard: &oms_lock::Guard) -> Result<()> {
        self.parent_write()?;
        row.object()?;
        let encoded = encoded(row)?;
        require(
            encoded.len() <= MAX_ROW,
            "refusing JSONL row larger than 1 MiB",
        )?;
        self.append_prepared(path, &encoded, guard)
    }
    fn append_prepared(&self, path: &Path, encoded: &[u8], guard: &oms_lock::Guard) -> Result<()> {
        self.check()?;
        require(
            !path.is_absolute()
                && path.components().all(|c| {
                    matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }),
            "runtime append path must stay inside its state root",
        )?;
        let root = self.owner(path).repo();
        let absolute = root.join(path);
        // Unlike replacements, appends publish against the current stream under
        // its lock, even when a command read an older generation hours earlier.
        let result = crate::io::append(self, &absolute, root, encoded, guard);
        self.invalidate(path);
        result
    }
    pub fn names(&self, path: &str) -> Result<Vec<std::ffi::OsString>> {
        self.owner(Path::new(path)).names(path, MAX_ROWS, true)
    }
    pub fn source_root(&self, path: &Path) -> &Path {
        if !path.is_absolute() {
            return self.owner(path).repo();
        }
        if path.strip_prefix(&self.state_repo).is_ok_and(|v| {
            v.components()
                .next()
                .is_some_and(|p| p.as_os_str() == ".oms")
        }) {
            &self.state_repo
        } else {
            &self.repo
        }
    }
    #[allow(deprecated)]
    pub fn expand_user(&self, raw: &std::ffi::OsStr) -> Result<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = raw.as_bytes();
        if !bytes.starts_with(b"~") {
            return Ok(PathBuf::from(raw));
        }
        let end = bytes.iter().position(|b| *b == b'/').unwrap_or(bytes.len());
        let home = if end == 1 {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .or_else(std::env::home_dir)
        } else {
            self.services
                .account_home(std::ffi::OsStr::from_bytes(&bytes[1..end]))?
        }
        .ok_or_else(|| error("Could not determine home directory."))?;
        Ok(home.join(std::ffi::OsStr::from_bytes(
            bytes
                .get(end + usize::from(end < bytes.len())..)
                .unwrap_or(&[]),
        )))
    }
    pub fn user_path(&self, raw: &std::ffi::OsStr) -> Result<PathBuf> {
        let path = self.expand_user(raw)?;
        let absolute = if path.is_absolute() {
            path
        } else {
            self.repo.join(path)
        };
        if let Ok(rel) = absolute.strip_prefix(&self.repo) {
            return Ok(rel.to_path_buf());
        }
        Ok(absolute)
    }
    pub fn read_optional_user(
        &self,
        raw: &std::ffi::OsStr,
        limit: usize,
    ) -> Result<Option<Vec<u8>>> {
        let p = self.user_path(raw)?;
        if !p.is_absolute() {
            return self.sources.optional(p, limit);
        }
        let root = oms_safe_fs::Root::open("/").map_err(|e| error(format!("input root: {e:?}")))?;
        match root.read_no_follow(
            &p,
            oms_safe_fs::ByteReadOptions {
                missing_ok: false,
                max_bytes: limit,
            },
        ) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(oms_safe_fs::ByteError::Missing) => Ok(None),
            Err(e) => Err(error(format!("input read: {e:?}"))),
        }
    }
    pub fn read_user(&self, raw: &std::ffi::OsStr, limit: usize) -> Result<Vec<u8>> {
        let selected = self.user_path(raw)?;
        let key = if selected.is_absolute() {
            selected
        } else {
            self.repo.join(selected)
        };
        if let Some(Some(bytes)) = self.cache.borrow().get(&key) {
            require(bytes.len() <= limit, "source exceeds read bound")?;
            return Ok(bytes.to_vec());
        }
        let bytes = self
            .read_optional_user(key.as_os_str(), limit)?
            .ok_or_else(|| error("input file is missing"))?;
        self.cache
            .borrow_mut()
            .insert(key, Some(Arc::new(bytes.clone())));
        Ok(bytes)
    }
    pub fn output_label(&self, path: &Path) -> Value {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            self.repo.join(path)
        };
        let physical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
        if let Ok(relative) = physical.strip_prefix(&self.repo) {
            return os(relative.as_os_str());
        }
        os(path.file_name().unwrap_or_default())
    }
    pub fn load_argument(&self, raw: &std::ffi::OsStr) -> Result<Value> {
        // common.load_json_argument checks a literal path in the invocation
        // directory before treating the argument as JSON. Tilde is literal here.
        let candidate = Path::new(raw);
        let absolute = if candidate.is_absolute() {
            candidate.to_owned()
        } else {
            self.cwd.join(candidate)
        };
        if std::fs::symlink_metadata(&absolute)
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            return parse(&self.read_user(absolute.as_os_str(), MAX_STATE)?);
        }
        parse(
            raw.to_str()
                .ok_or_else(|| error("expected JSON or a JSON file"))?
                .as_bytes(),
        )
    }
}
pub fn jsonl(raw: &[u8]) -> Result<Vec<Value>> {
    require(
        raw.len() <= MAX_STREAM,
        "append-only state exceeds the byte limit",
    )?;
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        require(line.len() <= MAX_ROW, "JSONL row exceeds 1 MiB")?;
        require(
            out.len() < MAX_ROWS,
            "append-only state exceeds the supported row limit",
        )?;
        let row = parse(line)?;
        row.object()?;
        out.push(row)
    }
    Ok(out)
}
pub fn safe_id(value: &str, label: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 160
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        format!("{label} must be a safe identifier"),
    )
}
pub fn random_id(prefix: &str) -> Result<String> {
    Ok(format!(
        "{prefix}{}",
        oms_command_plan::storage::random_hex(16)?
    ))
}
