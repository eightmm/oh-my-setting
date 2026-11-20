use crate::{Error, Result, notebook, util::*};
use oms_graph_python::{Parser, Session};
use oms_safe_fs::{Policy, Root};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const GRAPH_LIMIT: usize = 256 * 1024 * 1024;
const SCHEMA: u32 = 2;
const VERSION: u32 = 12;
const EXCLUDES: &[&str] = &[
    ".git",
    ".oms",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "target",
    "__pycache__",
];
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_bytes: i64,
    pub force: bool,
}
impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            include: vec![],
            exclude: vec![],
            max_bytes: 2 * 1024 * 1024,
            force: false,
        }
    }
}
impl BuildOptions {
    fn manifest(value: &Value) -> Self {
        Self {
            include: array(&value["discovery"], "include")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            exclude: array(&value["discovery"], "exclude")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            max_bytes: value["discovery"]["max_bytes"]
                .as_i64()
                .unwrap_or(2 * 1024 * 1024),
            force: false,
        }
    }
}
pub fn test_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts[..parts.len().saturating_sub(1)].iter().any(|p| {
        [
            "test",
            "tests",
            "testing",
            "__tests__",
            "e2e",
            "integration-tests",
            "integration_tests",
        ]
        .contains(&p.to_lowercase().as_str())
    }) || [
        "test_*.py",
        "*_test.py",
        "test_*.rs",
        "*_test.rs",
        "*_spec.py",
        "test-*.sh",
        "*-test.sh",
        "*_test.sh",
        "*-smoke.sh",
    ]
    .iter()
    .any(|p| glob(p, parts.last().copied().unwrap_or("")))
}
pub(super) fn language(path: &str) -> Option<(&'static str, u32)> {
    if path.to_lowercase().ends_with(".ipynb") {
        Some(("notebook", 1))
    } else {
        oms_graph::select(path).language_version()
    }
}
fn excluded(path: &str) -> bool {
    path.split('/').any(|p| EXCLUDES.contains(&p))
}
fn walk(root: &Path, rel: &Path, files: &mut BTreeSet<String>, cancel: &AtomicBool) -> Result<()> {
    crate::util::check(cancel)?;
    for entry in std::fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let path = rel.join(entry.file_name());
        let label = path.to_string_lossy().replace('\\', "/");
        if entry.file_type()?.is_dir() && !excluded(&label) {
            walk(root, &path, files, cancel)?;
        } else {
            files.insert(label);
        }
    }
    Ok(())
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct FileStat {
    #[serde(default)]
    mtime_sec: Option<i64>,
    #[serde(default)]
    mtime_nsec: Option<i64>,
    #[serde(default)]
    ino: Option<u64>,
    #[serde(default)]
    bytes: u64,
}
impl FileStat {
    #[allow(clippy::unnecessary_cast)]
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            mtime_sec: Some(stat.st_mtime),
            mtime_nsec: Some(stat.st_mtime_nsec as i64),
            ino: Some(stat.st_ino),
            bytes: stat.st_size as u64,
        }
    }
    fn reusable(&self, previous: &Value, started: i64) -> bool {
        self.mtime_sec.is_some_and(|time| time < started)
            && serde_json::from_value::<Self>(previous.clone()).is_ok_and(|old| old == *self)
    }
    fn add_to(&self, row: &mut Value) {
        row.as_object_mut().unwrap().extend(
            serde_json::to_value(self)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        );
    }
}
// Root resolves the repository physically. Traverse relative components with
// no-follow descriptors so a symlink swap cannot redirect a stat or probe.
fn source_parent(root: &Root, path: &str) -> std::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !safe_path(path) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = open(root.physical_root(), flags, Mode::empty())?;
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        dir = openat(&dir, *part, flags, Mode::empty())?;
    }
    Ok(dir)
}
fn source_stat(root: &Root, path: &str) -> std::io::Result<FileStat> {
    use rustix::fs::{AtFlags, FileType, statat};
    let parent = source_parent(root, path)?;
    let stat = statat(
        &parent,
        path.rsplit('/').next().unwrap(),
        AtFlags::SYMLINK_NOFOLLOW,
    )?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_size < 0 {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(FileStat::from_stat(&stat))
}
fn probe(root: &Root, path: &str, expected: &FileStat) -> std::io::Result<Vec<u8>> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
    use std::io::Read;
    let parent = source_parent(root, path)?;
    let fd = openat(
        &parent,
        path.rsplit('/').next().unwrap(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )?;
    let before = fstat(&fd)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || FileStat::from_stat(&before) != *expected
    {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file).take(8192).read_to_end(&mut bytes)?;
    let after = fstat(&file)?;
    if FileStat::from_stat(&after) != *expected
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
    {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(bytes)
}
struct Discovery {
    files: BTreeMap<String, (String, usize)>,
    stats: BTreeMap<String, FileStat>,
    skipped: Vec<Value>,
    build_started_sec: i64,
}
fn discover(repo: &Path, options: &BuildOptions, cancel: &AtomicBool) -> Result<Discovery> {
    discover_cached(repo, options, cancel, None)
}
fn discover_cached(
    repo: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    previous: Option<&Value>,
) -> Result<Discovery> {
    let build_started_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let previous_started = previous
        .and_then(|m| m["build_started_sec"].as_i64())
        .unwrap_or(i64::MIN);
    let old_skipped: BTreeMap<&str, &Value> = previous
        .map(|m| {
            array(m, "skipped")
                .iter()
                .map(|r| (s(r, "path"), r))
                .collect()
        })
        .unwrap_or_default();
    let mut candidates = BTreeSet::new();
    match command(
        repo,
        "git",
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        cancel,
    ) {
        Ok((0, raw, _)) => {
            for p in raw.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                candidates.insert(String::from_utf8_lossy(p).replace('\\', "/"));
            }
        }
        _ => walk(repo, Path::new(""), &mut candidates, cancel)?,
    }
    let root =
        Root::open(repo).map_err(|e| Error::new(format!("cannot open source root: {e:?}")))?;
    let mut files = BTreeMap::new();
    let mut skipped = vec![];
    let mut stats = BTreeMap::new();
    for path in candidates {
        crate::util::check(cancel)?;
        let name = path.rsplit('/').next().unwrap_or(&path);
        let mut reason = if excluded(&path) || !safe_path(&path) {
            "excluded"
        } else if [
            ".env", ".env.*", "*.pem", "*.key", "id_rsa*", "*.p12", "*.pfx",
        ]
        .iter()
        .any(|p| glob(p, name) || glob(p, &path))
        {
            "secret-name"
        } else if options.exclude.iter().any(|p| glob(p, &path))
            || (!options.include.is_empty() && !options.include.iter().any(|p| glob(p, &path)))
        {
            "excluded"
        } else {
            ""
        };
        let stat = if reason.is_empty() {
            source_stat(&root, &path).ok()
        } else {
            None
        };
        if reason.is_empty() {
            if options.max_bytes < 0 {
                reason = "too-large";
            } else if let Some(stat) = &stat {
                if stat.bytes > options.max_bytes as u64 {
                    reason = "too-large";
                } else if !options.force
                    && previous.is_some_and(|m| {
                        let row = &m["files"][&path];
                        let digest = s(row, "sha256");
                        stat.reusable(row, previous_started)
                            && digest.len() == 64
                            && digest.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    && language(&path).is_some()
                {
                    let old = &previous.unwrap()["files"][&path];
                    files.insert(path.clone(), (s(old, "sha256").into(), stat.bytes as usize));
                } else if let Some(old) = old_skipped.get(path.as_str()).filter(|old| {
                    !options.force
                        && stat.reusable(old, previous_started)
                        && (s(old, "reason") == "binary"
                            || (s(old, "reason") == "unparsed" && language(&path).is_none()))
                }) {
                    reason = if s(old, "reason") == "binary" {
                        "binary"
                    } else {
                        "unparsed"
                    };
                } else if language(&path).is_none() {
                    reason = match probe(&root, &path, stat) {
                        Ok(data) if data.contains(&0) => "binary",
                        Ok(_) => "unparsed",
                        Err(_) => "excluded",
                    };
                } else {
                    match root.read(
                        &path,
                        usize::try_from(options.max_bytes).unwrap_or(usize::MAX),
                        Policy::source(),
                    ) {
                        Ok(data) if source_stat(&root, &path).as_ref().ok() == Some(stat) => {
                            if data.iter().take(8192).any(|b| *b == 0) {
                                reason = "binary";
                            } else {
                                files.insert(path.clone(), (sha(&data), data.len()));
                            }
                        }
                        Ok(_) => {
                            return Err(Error::new(format!(
                                "source changed during discovery: {path}"
                            )));
                        }
                        Err(oms_safe_fs::Error::Oversize) => reason = "too-large",
                        Err(_) => reason = "excluded",
                    }
                }
            } else {
                reason = if repo.join(&path).is_symlink() {
                    "symlink"
                } else {
                    "excluded"
                };
            }
        }
        if !reason.is_empty() {
            let mut row = json!({"path":path,"reason":reason});
            if let Some(stat) = stat {
                stat.add_to(&mut row);
            }
            skipped.push(row);
        } else if let Some(stat) = stat {
            stats.insert(path, stat);
        }
    }
    Ok(Discovery {
        files,
        stats,
        skipped,
        build_started_sec,
    })
}
fn coverage(found: &Discovery) -> Value {
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut extensions: BTreeMap<String, usize> = BTreeMap::new();
    for row in &found.skipped {
        *reasons.entry(s(row, "reason").into()).or_default() += 1;
        if s(row, "reason") == "unparsed" {
            let extension = Path::new(s(row, "path"))
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
                .unwrap_or_else(|| "<none>".into());
            *extensions.entry(extension).or_default() += 1;
        }
    }
    json!({"parsed":found.files.len(),"unparsed":reasons.get("unparsed").copied().unwrap_or(0),"unparsed_by_extension":extensions,"skipped_by_reason":reasons})
}
struct Python<'a> {
    session: Option<Session>,
    helper: Option<PathBuf>,
    max_bytes: usize,
    cancel: &'a AtomicBool,
}
impl Parser for Python<'_> {
    fn parse(
        &mut self,
        path: &str,
        text: &str,
        digest: &str,
    ) -> std::result::Result<oms_graph_python::ParseResult, oms_graph_python::Error> {
        if text.len() > oms_graph_python::MAX_SOURCE_BYTES {
            return Err(oms_graph_python::Error::SourceTooLarge {
                bytes: text.len(),
                max_bytes: oms_graph_python::MAX_SOURCE_BYTES,
            });
        }
        if self.session.is_some() && text.len() > self.max_bytes {
            self.session = None;
        }
        if self.session.is_none() {
            self.max_bytes = text.len().max(oms_graph_python::DEFAULT_MAX_BYTES);
            let helper = self.helper.as_deref().ok_or_else(|| {
                oms_graph_python::Error::HelperUnavailable(
                    "packaged helper path was not supplied".into(),
                )
            })?;
            self.session = Some(Session::start(
                helper,
                oms_graph_python::Options {
                    max_bytes: self.max_bytes,
                    ..Default::default()
                },
            )?);
        }
        let cancel = oms_graph_python::CancelFlag::new();
        let done = (std::sync::Mutex::new(false), std::sync::Condvar::new());
        struct Completion<'a>(&'a (std::sync::Mutex<bool>, std::sync::Condvar));
        impl Drop for Completion<'_> {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
                self.0.1.notify_all();
            }
        }
        std::thread::scope(|scope| {
            let monitor = scope.spawn(|| {
                let mut finished = done.0.lock().unwrap_or_else(|e| e.into_inner());
                while !*finished {
                    if self.cancel.load(Ordering::SeqCst) {
                        cancel.cancel();
                        break;
                    }
                    finished = done
                        .1
                        .wait_timeout(finished, std::time::Duration::from_millis(20))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            });
            let completion = Completion(&done);
            let result = self
                .session
                .as_mut()
                .unwrap()
                .parse_cancellable(path, text, digest, &cancel);
            drop(completion);
            let _ = monitor.join();
            result
        })
    }
}
fn helper(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    let path = explicit.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("OMS_GRAPH_PYTHON_HELPER")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });
    if let Some(ref path) = path {
        if !path.is_absolute() {
            return Err(Error::new("Python parser helper path must be absolute"));
        }
        let info = std::fs::symlink_metadata(path)?;
        use std::os::unix::fs::MetadataExt;
        if !info.is_file()
            || info.mode() & 0o022 != 0
            || info.mode() & 0o111 == 0
            || (info.uid() != 0 && info.uid() != rustix::process::geteuid().as_raw())
        {
            return Err(Error::new(
                "Python parser helper is not a trusted executable",
            ));
        }
    }
    Ok(path)
}
fn extraction(path: &str, data: &[u8], python: &mut impl Parser) -> Result<Value> {
    let (lang, version) =
        language(path).ok_or_else(|| Error::new(format!("no parser for {path}")))?;
    let digest = sha(data);
    let text = String::from_utf8_lossy(data);
    let mut parsed = if lang == "notebook" {
        notebook::parse(path, &text, &digest, python).map_err(|e| Error::new(e.to_string()))?
    } else {
        oms_graph::parse(path, &text, &digest, python)
            .map_err(|e| Error::new(format!("parser failed: {e:?}")))?
    };
    if lang == "rust" {
        let crate_dir = path
            .split_once("/src/")
            .or_else(|| path.split_once("/tests/"))
            .map(|(dir, _)| dir)
            .or_else(|| (path.starts_with("src/") || path.starts_with("tests/")).then_some(""));
        if let Some(dir) = crate_dir {
            let manifest = if dir.is_empty() {
                "Cargo.toml".to_owned()
            } else {
                format!("{dir}/Cargo.toml")
            };
            parsed.refs.push(json!({"from":format!("file:{path}"),"relation":"imports","kind":"path","value":manifest,"line":1}));
        }
    }
    let kind = if test_path(path) { "test" } else { "file" };
    let id = format!("{kind}:{path}");
    if kind == "test" {
        for edge in &mut parsed.edges {
            if s(edge, "source") == format!("file:{path}") {
                edge["source"] = json!(id);
            }
        }
        for reference in &mut parsed.refs {
            if s(reference, "from") == format!("file:{path}") {
                reference["from"] = json!(id);
            }
        }
        if lang != "notebook" {
            let pattern =
                regex::Regex::new(r"[A-Za-z0-9_][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_][A-Za-z0-9_.-]*)+")
                    .unwrap();
            for (line, text) in text.lines().enumerate() {
                for m in pattern.find_iter(text) {
                    if text[..m.start()]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && !"_./$\\-".contains(c))
                    {
                        parsed.refs.push(json!({"from":id,"relation":"tests","kind":"path","value":m.as_str().trim_end_matches(['.',',',':',';','`','\'', '"']),"line":line+1}));
                    }
                }
            }
        }
    }
    parsed.nodes.insert(0,json!({"id":id,"kind":kind,"name":path.rsplit('/').next().unwrap_or(path),"path":path,"language":lang,"source_digest":digest,"summary":null,"crux":null,"metadata":{}}));
    Ok(
        json!({"path":path,"sha256":digest,"bytes":data.len(),"parser":lang,"parser_version":version,"nodes":parsed.nodes,"edges":parsed.edges,"refs":parsed.refs}),
    )
}
pub fn build(
    repo: &Path,
    state: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    explicit_helper: Option<&Path>,
) -> Result<Value> {
    let found = discover(repo, options, cancel)?;
    Ok(build_from(repo, state, options, cancel, explicit_helper, found)?.0)
}
fn cache_private_files(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::set_permissions(PathBuf::from(name), std::fs::Permissions::from_mode(0o600))
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn cache_open(state: &Path, writable: bool) -> Result<Option<rusqlite::Connection>> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = state.join("cache.sqlite3");
    no_symlinks(&path)?;
    if writable {
        std::fs::create_dir_all(state)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::new("invalid graph cache file"));
        }
        cache_private_files(&path)?;
    } else if !path.exists() {
        return Ok(None);
    }
    let flags = if writable {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    } | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let db = rusqlite::Connection::open_with_flags(&path, flags).map_err(cache_error)?;
    if writable {
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS file_cache (cache_key TEXT PRIMARY KEY, path TEXT NOT NULL, sha256 TEXT NOT NULL, version INTEGER NOT NULL, schema_ver INTEGER NOT NULL, payload BLOB NOT NULL);").map_err(cache_error)?;
        cache_private_files(&path)?;
    }
    Ok(Some(db))
}
fn cache_error(e: rusqlite::Error) -> Error {
    Error::new(format!("project extraction cache: {e}"))
}
fn cache_get(
    db: &rusqlite::Connection,
    key: &str,
    path: &str,
    digest: &str,
    version: u32,
) -> Option<Value> {
    let data: Vec<u8> = db.query_row("SELECT payload FROM file_cache WHERE cache_key=?1 AND path=?2 AND sha256=?3 AND version=?4 AND schema_ver=?5", rusqlite::params![key,path,digest,version,SCHEMA], |row| row.get(0)).ok()?;
    if data.len() > 8 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice::<Value>(&data).ok().filter(|v| {
        v["path"] == path
            && v["sha256"] == digest
            && v["parser_version"] == version
            && v["nodes"].is_array()
            && v["edges"].is_array()
            && v["refs"].is_array()
    })
}

fn build_from(
    repo: &Path,
    state: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    explicit_helper: Option<&Path>,
    found: Discovery,
) -> Result<(Value, super::index::Graph)> {
    let mut settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
    settings.cwd = repo.to_path_buf();
    let lock = oms_lock::Lock::for_file(&state.join("manifest.json"), settings)
        .map_err(|e| Error::new(e.to_string()))?;
    let guard = lock.try_acquire().map_err(|e| Error::new(e.to_string()))?;
    let mut python = Python {
        session: None,
        helper: helper(explicit_helper)?,
        max_bytes: 0,
        cancel,
    };
    let mut files = BTreeMap::new();
    let mut extractions = vec![];
    let mut cached = 0;
    let legacy = state.join("cache");
    let mut db = cache_open(state, true)?.expect("writable cache");
    let transaction = db.transaction().map_err(cache_error)?;
    transaction
        .execute_batch("CREATE TEMP TABLE wanted_cache (cache_key TEXT PRIMARY KEY);")
        .map_err(cache_error)?;
    let root = Root::open(repo).map_err(|e| Error::new(format!("source root: {e:?}")))?;
    for (path, (digest, size)) in &found.files {
        crate::util::check(cancel)?;
        let (lang, version) = language(path).unwrap();
        let key = sha(format!("{path}\n{digest}\n{version}\n{VERSION}\n{SCHEMA}"));
        transaction
            .execute("INSERT INTO wanted_cache VALUES (?1)", [&key])
            .map_err(cache_error)?;
        let old = if options.force {
            None
        } else {
            cache_get(&transaction, &key, path, digest, version)
        };
        let was_cached = old.is_some();
        let payload = match old {
            Some(v) => {
                cached += 1;
                v
            }
            None => {
                let data = root
                    .read(
                        path,
                        usize::try_from(options.max_bytes).unwrap_or(0),
                        Policy::source(),
                    )
                    .map_err(|e| {
                        Error::new(format!("source changed during build: {path}: {e:?}"))
                    })?;
                if data.len() != *size || sha(&data) != *digest {
                    return Err(Error::new(format!("source changed during build: {path}")));
                }
                let v = extraction(path, &data, &mut python)?;
                let raw = bytes(&v, false)?;
                if raw.len() <= 8 * 1024 * 1024 {
                    transaction
                        .execute(
                            "INSERT OR REPLACE INTO file_cache VALUES (?1,?2,?3,?4,?5,?6)",
                            rusqlite::params![key, path, digest, version, SCHEMA, raw],
                        )
                        .map_err(cache_error)?;
                }
                v
            }
        };
        let mut row = json!({"sha256":digest,"bytes":size,"parser":lang,"cached":was_cached});
        if let Some(stat) = found.stats.get(path) {
            stat.add_to(&mut row);
        }
        files.insert(path.clone(), row);
        extractions.push(oms_graph_resolve::Extraction {
            path: path.clone(),
            sha256: digest.clone(),
            parser: lang.into(),
            nodes: array(&payload, "nodes").to_vec(),
            edges: array(&payload, "edges").to_vec(),
            refs: array(&payload, "refs").to_vec(),
        });
    }
    let (nodes, edges) =
        oms_graph_resolve::resolve(&extractions).map_err(|e| Error::new(e.to_string()))?;
    let revision_source = files
        .iter()
        .map(|(path, v)| format!("{path}\t{}", s(v, "sha256")))
        .collect::<Vec<_>>()
        .join("\n")
        + &format!("\n{VERSION}\n{SCHEMA}");
    let revision = sha(revision_source);
    let graph = json!({"schema":SCHEMA,"revision":revision,"nodes":nodes,"edges":edges});
    let mut include = options.include.clone();
    include.sort();
    let mut exclude = options.exclude.clone();
    exclude.sort();
    let manifest = json!({"schema":SCHEMA,"revision":revision,"generated_at":now(),"build_started_sec":found.build_started_sec,"parser_version":VERSION,"discovery":{"include":include,"exclude":exclude,"max_bytes":options.max_bytes},"files":files,"skipped":found.skipped,"coverage":coverage(&found),"stats":{"files":files.len(),"nodes":nodes.len(),"edges":edges.len(),"cached":cached,"parsed":files.len()-cached}});
    guard.check().map_err(|e| Error::new(e.to_string()))?;
    if state.starts_with(repo.join(".oms")) {
        create_marker(repo)?;
    }
    write_json(&state.join("graph.json"), &graph)?;
    super::sqlite::write(state, &graph)?;
    write_json(&state.join("manifest.json"), &manifest)?;
    let index = super::index::Graph::new(graph)?;
    let brief = super::index::orientation(
        &index,
        &manifest,
        repo.file_name().and_then(|s| s.to_str()).unwrap_or(""),
    );
    write_json(
        &state.join("orientation.json"),
        &json!({"schema":1,"revision":revision,"generated_at":now(),"text":brief}),
    )?;
    transaction
        .execute(
            "DELETE FROM file_cache WHERE cache_key NOT IN (SELECT cache_key FROM wanted_cache)",
            [],
        )
        .map_err(cache_error)?;
    transaction.commit().map_err(cache_error)?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(cache_error)?;
    cache_private_files(&state.join("cache.sqlite3"))?;
    no_symlinks(&legacy)?;
    if std::fs::symlink_metadata(&legacy).is_ok_and(|m| m.is_dir() && !m.is_symlink()) {
        std::fs::remove_dir_all(&legacy)?;
    }
    Ok((manifest, index))
}
pub fn load(state: &Path) -> Result<Value> {
    read_json(&state.join("graph.json"), GRAPH_LIMIT)?
        .filter(Value::is_object)
        .ok_or_else(|| Error::new("project graph has not been built; run: oms graph project build"))
}
pub fn check(repo: &Path, state: &Path, cancel: &AtomicBool) -> Result<Value> {
    Ok(freshness(repo, state, cancel, false)?.status)
}
struct Freshness {
    status: Value,
    found: Option<Discovery>,
    manifest: Option<Value>,
    graph: Option<Value>,
    db: Option<rusqlite::Connection>,
}
fn freshness(
    repo: &Path,
    state: &Path,
    cancel: &AtomicBool,
    prefer_sql: bool,
) -> Result<Freshness> {
    let Some(manifest) =
        read_json(&state.join("manifest.json"), GRAPH_LIMIT)?.filter(Value::is_object)
    else {
        return Ok(Freshness {
            status: json!({"present":false,"fresh":false,"revision":"","stale":[],"missing":[],"new":[],"coverage":{"parsed":0,"unparsed":0,"unparsed_by_extension":{},"skipped_by_reason":{}}}),
            found: None,
            manifest: None,
            graph: None,
            db: None,
        });
    };
    let found = discover_cached(
        repo,
        &BuildOptions::manifest(&manifest),
        cancel,
        Some(&manifest),
    )?;
    let current: BTreeSet<&str> = found.files.keys().map(String::as_str).collect();
    let known: BTreeSet<&str> = manifest["files"]
        .as_object()
        .map(|v| v.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let stale: Vec<&str> = current
        .intersection(&known)
        .copied()
        .filter(|p| found.files[*p].0 != s(&manifest["files"][*p], "sha256"))
        .collect();
    let missing: Vec<&str> = known.difference(&current).copied().collect();
    let new: Vec<&str> = current.difference(&known).copied().collect();
    let mut outdated = json!({});
    for (key, expected) in [("parser_version", VERSION), ("schema", SCHEMA)] {
        if manifest[key] != expected {
            outdated[key] = json!({"built":manifest[key],"current":expected});
        }
    }
    let db = if prefer_sql {
        super::sqlite::open_verified(state, s(&manifest, "revision"))
    } else {
        None
    };
    let sql_valid = db.is_some();
    let graph = if sql_valid { None } else { load(state).ok() };
    let valid = sql_valid
        || graph.as_ref().is_some_and(|g| {
            g["schema"] == SCHEMA
                && g["revision"] == manifest["revision"]
                && !s(g, "revision").is_empty()
                && g["nodes"].is_array()
                && g["edges"].is_array()
        });
    let status = json!({"present":true,"fresh":stale.is_empty()&&missing.is_empty()&&new.is_empty()&&outdated.as_object().unwrap().is_empty()&&valid,"revision":manifest["revision"],"stale":stale,"missing":missing,"new":new,"outdated":outdated,"coverage":coverage(&found)});
    Ok(Freshness {
        status,
        found: Some(found),
        manifest: Some(manifest),
        graph,
        db,
    })
}
pub fn ensure(
    repo: &Path,
    state: &Path,
    max_files: i64,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    Ok(ensure_loaded(repo, state, max_files, cancel, helper, false)?.status)
}
pub(super) struct Loaded {
    pub status: Value,
    pub manifest: Value,
    pub graph: Option<super::index::Graph>,
    pub db: Option<rusqlite::Connection>,
}
pub(super) fn ensure_loaded(
    repo: &Path,
    state: &Path,
    max_files: i64,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    prefer_sql: bool,
) -> Result<Loaded> {
    let Freshness {
        status,
        found,
        manifest,
        graph,
        db,
    } = freshness(repo, state, cancel, prefer_sql)?;
    if status["fresh"] == true {
        return Ok(Loaded {
            status: json!({"action":"fresh","revision":status["revision"]}),
            manifest: manifest.unwrap(),
            graph: graph.map(super::index::Graph::new).transpose()?,
            db,
        });
    }
    let present = status["present"] == true;
    let options = manifest
        .as_ref()
        .map(BuildOptions::manifest)
        .unwrap_or_default();
    let found = match found {
        Some(found) => found,
        None => discover(repo, &options, cancel)?,
    };
    if !present && found.files.len() as i128 > max_files as i128 {
        return Err(Error::new(format!(
            "project graph auto-build skipped: {} files exceed the {max_files}-file bound; run: oms graph project build",
            found.files.len()
        )));
    }
    let (manifest, graph) = build_from(repo, state, &options, cancel, helper, found)?;
    Ok(Loaded {
        status: json!({"action":if present {"refreshed"} else {"built"},"revision":manifest["revision"],"stats":manifest["stats"],"skipped":array(&manifest,"skipped").len()}),
        manifest,
        graph: Some(graph),
        db: None,
    })
}

pub(super) fn revision_nodes(
    path: &str,
    text: &str,
    explicit_helper: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<Vec<Value>> {
    let mut python = Python {
        session: None,
        helper: helper(explicit_helper)?,
        max_bytes: 0,
        cancel,
    };
    Ok(array(&extraction(path, text.as_bytes(), &mut python)?, "nodes").to_vec())
}

type Groups = Option<oms_command_plan::services::GroupResolver>;

fn candidate(path: &str, options: &BuildOptions) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    safe_path(path)
        && !excluded(path)
        && language(path).is_some()
        && ![
            ".env", ".env.*", "*.pem", "*.key", "id_rsa*", "*.p12", "*.pfx",
        ]
        .iter()
        .any(|pattern| glob(pattern, name) || glob(pattern, path))
        && !options.exclude.iter().any(|pattern| glob(pattern, path))
        && (options.include.is_empty() || options.include.iter().any(|pattern| glob(pattern, path)))
}

fn candidates(
    repo: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
) -> Result<(BTreeSet<String>, bool)> {
    let mut pending = vec![PathBuf::new()];
    let mut paths = BTreeSet::new();
    let mut visited = 0usize;
    while let Some(dir) = pending.pop() {
        crate::util::check(cancel)?;
        let entries = match std::fs::read_dir(repo.join(&dir)) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut entries = entries
            .take(9001 - visited)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            crate::util::check(cancel)?;
            visited += 1;
            if visited > 9000 {
                return Ok((paths, true));
            }
            let path = dir.join(entry.file_name());
            let Some(label) = path.to_str() else { continue };
            let kind = entry.file_type()?;
            if kind.is_dir() && !excluded(label) && safe_path(label) {
                pending.push(path);
            } else if kind.is_file() && candidate(label, options) {
                paths.insert(label.into());
            }
        }
    }
    Ok((paths, false))
}

pub(super) fn read_cached_graph(
    repo: &Path,
    state: &Path,
    groups: Groups,
    cancel: &AtomicBool,
) -> Result<Option<Value>> {
    let root = Root::open(repo).map_err(|e| Error::new(format!("source root: {e:?}")))?;
    let state = if let Ok(relative) = state.strip_prefix(repo) {
        root.physical_root().join(relative)
    } else {
        state.to_path_buf()
    };
    no_symlinks(&state)?;
    match std::fs::symlink_metadata(&state) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let (store, prefix) = match state.strip_prefix(root.physical_root()) {
        Ok(relative) => (
            oms_command_plan::storage::Store::open(root.physical_root())?
                .with_group_resolver(groups.or_else(crate::util::current_group_resolver)),
            relative.to_path_buf(),
        ),
        Err(_) => (
            oms_command_plan::storage::Store::open(&state)?
                .with_group_resolver(groups.or_else(crate::util::current_group_resolver)),
            PathBuf::new(),
        ),
    };
    let manifest = store.optional(prefix.join("manifest.json"), GRAPH_LIMIT)?;
    let graph = store.optional(prefix.join("graph.json"), GRAPH_LIMIT)?;
    let (Some(manifest), Some(graph)) = (manifest, graph) else {
        return Ok(None);
    };
    let (Ok(manifest), Ok(graph)) = (
        serde_json::from_slice::<Value>(&manifest),
        serde_json::from_slice::<Value>(&graph),
    ) else {
        return Ok(None);
    };
    let options = BuildOptions::manifest(&manifest);
    if !manifest["discovery"]["include"].is_array()
        || !manifest["discovery"]["exclude"].is_array()
        || !manifest["discovery"]["max_bytes"].is_i64()
        || !options.include.is_empty()
        || !options.exclude.is_empty()
        || manifest["schema"] != SCHEMA
        || manifest["parser_version"] != VERSION
        || graph["schema"] != SCHEMA
        || graph["revision"] != manifest["revision"]
        || s(&graph, "revision").is_empty()
        || !graph["nodes"].is_array()
        || !graph["edges"].is_array()
    {
        return Ok(None);
    }
    let (paths, capped) = candidates(root.physical_root(), &options, cancel)?;
    if capped {
        return Ok(None);
    }
    let Some(files) = manifest["files"].as_object() else {
        return Ok(None);
    };
    let mut found = BTreeMap::new();
    for path in paths {
        crate::util::check(cancel)?;
        if let Ok(data) = root.read(
            &path,
            usize::try_from(options.max_bytes).unwrap_or(0),
            Policy::source(),
        ) && !data.iter().take(8192).any(|b| *b == 0)
            && std::str::from_utf8(&data).is_ok()
        {
            found.insert(path, sha(&data));
        }
    }
    if found.len() != files.len()
        || found
            .iter()
            .any(|(path, digest)| files.get(path).is_none_or(|row| s(row, "sha256") != digest))
    {
        return Ok(None);
    }
    Ok(Some(graph))
}

pub(super) fn fallback_graph(
    repo: &Path,
    state: &Path,
    graph: &Value,
    targets: &[String],
    helper_path: Option<&Path>,
    groups: Groups,
    cancel: &AtomicBool,
) -> Result<Value> {
    let root = Root::open(repo).map_err(|e| Error::new(format!("source root: {e:?}")))?;
    let options = BuildOptions::default();
    let (mut paths, capped) = candidates(root.physical_root(), &options, cancel)?;
    if capped {
        for dir in ["tests", "test"] {
            let directory = root.physical_root().join(dir);
            if !std::fs::symlink_metadata(&directory).is_ok_and(|m| m.is_dir() && !m.is_symlink()) {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(directory) {
                for entry in entries.flatten().take(256) {
                    crate::util::check(cancel)?;
                    if !entry.file_type().is_ok_and(|t| t.is_file()) {
                        continue;
                    }
                    let path = Path::new(dir).join(entry.file_name());
                    if let Some(path) = path.to_str().filter(|p| candidate(p, &options)) {
                        paths.insert(path.into());
                    }
                }
            }
        }
    }
    paths.extend(array(graph, "nodes").iter().filter_map(|node| {
        let path = s(node, "path");
        if matches!(s(node, "kind"), "file" | "test") && candidate(path, &options) {
            Some(path.to_owned())
        } else {
            None
        }
    }));
    paths.extend(targets.iter().cloned());
    let mut sibling_budget = 488usize;
    for target in targets {
        let parent = Path::new(target).parent().unwrap_or(Path::new(""));
        if let Ok(entries) = std::fs::read_dir(root.physical_root().join(parent)) {
            for entry in entries.flatten().take(sibling_budget) {
                sibling_budget -= 1;
                crate::util::check(cancel)?;
                if !entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let sibling = parent.join(entry.file_name());
                if let Some(name) = sibling.to_str().filter(|name| candidate(name, &options)) {
                    paths.insert(name.to_owned());
                }
            }
        }
    }
    let state = if let Ok(relative) = state.strip_prefix(repo) {
        root.physical_root().join(relative)
    } else {
        state.to_path_buf()
    };
    let _ = groups;
    let cache = cache_open(&state, false).ok().flatten();
    let mut python = Python {
        session: None,
        helper: helper(helper_path)?,
        max_bytes: 0,
        cancel,
    };
    let mut records = Vec::new();
    for path in paths {
        crate::util::check(cancel)?;
        let target = targets.contains(&path);
        let limit = if target {
            oms_graph_python::MAX_SOURCE_BYTES
        } else {
            options.max_bytes as usize
        };
        let data = match root.read(&path, limit, Policy::source()) {
            Ok(data) => data,
            Err(e) if target => {
                return Err(Error::new(format!("cannot read target {path}: {e:?}")));
            }
            Err(_) => continue,
        };
        if std::str::from_utf8(&data).is_err()
            || data
                .iter()
                .take(if target { data.len() } else { 8192 })
                .any(|b| *b == 0)
        {
            if target {
                return Err(Error::new(format!("malformed target: {path}")));
            }
            continue;
        }
        let (lang, version) = language(&path).ok_or_else(|| Error::new("no target parser"))?;
        let digest = sha(&data);
        let key = sha(format!("{path}\n{digest}\n{version}\n{VERSION}\n{SCHEMA}"));
        let cached = cache
            .as_ref()
            .and_then(|db| cache_get(db, &key, &path, &digest, version));
        let parsed = match cached {
            Some(v) => v,
            None => match extraction(&path, &data, &mut python) {
                Ok(v) => v,
                Err(e) if target => return Err(e),
                Err(_) => continue,
            },
        };
        records.push(oms_graph_resolve::Extraction {
            path,
            sha256: digest,
            parser: lang.into(),
            nodes: array(&parsed, "nodes").to_vec(),
            edges: array(&parsed, "edges").to_vec(),
            refs: array(&parsed, "refs").to_vec(),
        });
    }
    let (nodes, edges) =
        oms_graph_resolve::resolve(&records).map_err(|e| Error::new(e.to_string()))?;
    Ok(json!({"nodes":nodes,"edges":edges}))
}
