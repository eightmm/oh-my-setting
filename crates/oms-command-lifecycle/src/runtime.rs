use crate::Output;
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) const LIMIT: usize = 16 * 1024 * 1024;
#[derive(Debug)]
pub(crate) struct Error(pub i32, pub String);
pub(crate) type Result<T> = std::result::Result<T, Error>;
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(2, format!("filesystem operation failed: {e}"))
    }
}
pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(2, message.into()))
}
pub(crate) fn check(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Acquire) {
        Err(Error(130, "lifecycle interrupted".into()))
    } else {
        Ok(())
    }
}
pub(crate) fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
pub(crate) fn now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::from_timestamp(t.as_secs() as i64, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn text(s: impl AsRef<str>) -> Output {
    (0, format!("{}\n", s.as_ref()).into_bytes(), Vec::new())
}
pub(crate) fn json(v: &Value) -> Result<Output> {
    Ok((0, bytes(v)?, Vec::new()))
}
pub(crate) fn parse(raw: &[u8]) -> Result<Value> {
    fn unique(v: &oms_context::Json) -> bool {
        use oms_context::Json as J;
        match v {
            J::Object(rows) => {
                let mut seen = BTreeSet::new();
                rows.iter().all(|(k, v)| seen.insert(k) && unique(v))
            }
            J::Array(rows) => rows.iter().all(unique),
            _ => true,
        }
    }
    let v =
        oms_context::parse_json(raw).map_err(|_| Error(2, "invalid or oversized JSON".into()))?;
    if !unique(&v) {
        return fail("duplicate JSON member");
    }
    serde_json::from_slice(raw).map_err(|_| Error(2, "invalid JSON value".into()))
}
pub(crate) fn bytes(v: &Value) -> Result<Vec<u8>> {
    fn value(v: &Value) -> oms_json::Value {
        use oms_json::Value as J;
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::String(s) => J::String(s.as_str().into()),
            Value::Number(n) => {
                J::integer(&n.to_string()).unwrap_or_else(|_| J::Float(n.as_f64().unwrap_or(0.0)))
            }
            Value::Array(a) => J::Array(a.iter().map(value).collect()),
            Value::Object(o) => J::Object(
                o.iter()
                    .map(|(k, v)| (k.as_str().into(), value(v)))
                    .collect(),
            ),
        }
    }
    let mut b = oms_json::pretty_json(&value(v))
        .map_err(|_| Error(2, "JSON serialization failed".into()))?;
    b.push(b'\n');
    Ok(b)
}
pub(crate) fn canonical_ascii(v: &Value) -> Result<Vec<u8>> {
    fn ordered(v: &Value) -> Value {
        match v {
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (k.clone(), ordered(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(ordered).collect()),
            _ => v.clone(),
        }
    }
    let raw = serde_json::to_string(&ordered(v))
        .map_err(|_| Error(2, "canonical JSON serialization failed".into()))?;
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii() {
            out.push(ch)
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    Ok(out.into_bytes())
}
pub(crate) fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
pub(crate) fn physical(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return fail("expected an absolute path");
    }
    let mut ancestor = path;
    let mut names = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                names.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| Error(2, "invalid path".into()))?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error(2, "invalid ancestor".into()))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let mut result = fs::canonicalize(ancestor)?;
    for name in names.iter().rev() {
        result.push(name);
    }
    Ok(result)
}
pub(crate) fn leaf(path: &Path) -> Result<PathBuf> {
    Ok(physical(
        path.parent()
            .ok_or_else(|| Error(2, "missing parent".into()))?,
    )?
    .join(
        path.file_name()
            .ok_or_else(|| Error(2, "missing leaf".into()))?,
    ))
}
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let path = leaf(path)?;
    let root =
        Root::open(path.parent().unwrap()).map_err(|_| Error(2, "unsafe read root".into()))?;
    root.read_no_follow(
        &path,
        ByteReadOptions {
            missing_ok: false,
            max_bytes: limit,
        },
    )
    .map_err(|e| Error(2, format!("unsafe or unreadable file: {e:?}")))
}
pub(crate) fn value(path: &Path) -> Result<Value> {
    parse(&read(path, LIMIT)?)
}
/// Caller holds the canonical state lock across the complete mutation.
pub(crate) fn atomic(path: &Path, raw: &[u8]) -> Result<()> {
    let path = leaf(path)?;
    let mut ancestor = path.parent().unwrap();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error(2, "missing storage ancestor".into()))?;
    }
    let root = Root::open(ancestor).map_err(|_| Error(2, "unsafe storage root".into()))?;
    root.atomic_mutate(
        &path,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: LIMIT,
            },
            create_parent: true,
            max_output: LIMIT,
            ..Default::default()
        },
        |_| Ok::<_, ()>(raw.to_vec()),
    )
    .map_err(|e| {
        Error(
            2,
            format!("atomic publication requires reconciliation: {e:?}"),
        )
    })?;
    Ok(())
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if !exists(path) {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o022 != 0
    {
        return fail("unsafe lifecycle state directory");
    }
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn file_lock(path: &Path, cwd: &Path) -> Result<oms_lock::Lock> {
    let mut settings =
        oms_lock::Settings::from_env().map_err(|e| Error(2, format!("lock settings: {e:?}")))?;
    settings.cwd = cwd.to_path_buf();
    oms_lock::Lock::for_file(&leaf(path)?, settings)
        .map_err(|e| Error(2, format!("lock derivation: {e:?}")))
}
pub(crate) fn lock(path: &Path, cwd: &Path) -> Result<oms_lock::Guard> {
    file_lock(path, cwd)?
        .acquire(None)
        .map_err(|e| Error(75, format!("lifecycle lock unavailable: {e:?}")))
}
pub(crate) fn find(name: &str, cwd: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| {
            if p.is_absolute() {
                p.join(name)
            } else {
                cwd.join(p).join(name)
            }
        })
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0))
}
fn capture_file() -> Result<fs::File> {
    let directory = Scratch::new()?;
    let name = directory.0.join("capture");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&name)?;
    fs::remove_file(&name)?;
    Ok(file)
}
pub(crate) fn command(
    program: impl AsRef<OsStr>,
    args: &[OsString],
    cwd: &Path,
    input: Option<&[u8]>,
    flag: &AtomicBool,
    timeout: Duration,
) -> Result<Output> {
    command_env(program, args, cwd, input, flag, timeout, &[])
}
pub(crate) fn command_env(
    program: impl AsRef<OsStr>,
    args: &[OsString],
    cwd: &Path,
    input: Option<&[u8]>,
    flag: &AtomicBool,
    timeout: Duration,
    environment: &[(&str, &OsStr)],
) -> Result<Output> {
    command_env_limit(program, args, cwd, input, flag, timeout, environment, LIMIT)
}
pub(crate) fn command_without_env(
    program: impl AsRef<OsStr>,
    args: &[OsString],
    cwd: &Path,
    input: Option<&[u8]>,
    flag: &AtomicBool,
    timeout: Duration,
    removed: &[&str],
) -> Result<Output> {
    command_environment(
        program,
        args,
        cwd,
        input,
        flag,
        timeout,
        &[],
        removed,
        LIMIT,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn command_env_limit(
    program: impl AsRef<OsStr>,
    args: &[OsString],
    cwd: &Path,
    input: Option<&[u8]>,
    flag: &AtomicBool,
    timeout: Duration,
    environment: &[(&str, &OsStr)],
    limit: usize,
) -> Result<Output> {
    command_environment(
        program,
        args,
        cwd,
        input,
        flag,
        timeout,
        environment,
        &[],
        limit,
    )
}
#[allow(clippy::too_many_arguments)]
fn command_environment(
    program: impl AsRef<OsStr>,
    args: &[OsString],
    cwd: &Path,
    input: Option<&[u8]>,
    flag: &AtomicBool,
    timeout: Duration,
    environment: &[(&str, &OsStr)],
    removed: &[&str],
    limit: usize,
) -> Result<Output> {
    check(flag)?;
    let mut out = capture_file()?;
    let mut err = capture_file()?;
    let stdin = if let Some(raw) = input {
        let mut f = capture_file()?;
        f.write_all(raw)?;
        f.seek(SeekFrom::Start(0))?;
        Stdio::from(f)
    } else {
        Stdio::null()
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .process_group(0)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(stdin)
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(err.try_clone()?));
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    for name in removed {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|e| Error(2, format!("cannot start command: {e}")))?;
    let started = Instant::now();
    let mut code = None;
    let status = loop {
        let over = out.metadata()?.len() > limit as u64 || err.metadata()?.len() > limit as u64;
        if flag.load(Ordering::Acquire) || started.elapsed() >= timeout || over {
            code = Some(if flag.load(Ordering::Acquire) {
                130
            } else if over {
                2
            } else {
                124
            });
            if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
            break child.wait()?;
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    out.seek(SeekFrom::Start(0))?;
    err.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    out.take(limit as u64 + 1).read_to_end(&mut stdout)?;
    err.take(limit as u64 + 1).read_to_end(&mut stderr)?;
    if stdout.len() > limit || stderr.len() > limit {
        return fail("command output exceeds its byte limit");
    }
    Ok((code.unwrap_or(status.code().unwrap_or(1)), stdout, stderr))
}

/// Exclusive, private scratch storage; probe configuration never uses a live home.
pub(crate) struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let base = physical(&std::env::temp_dir())?;
        for _ in 0..128 {
            let mut entropy = [0u8; 16];
            fs::File::open("/dev/urandom")?.read_exact(&mut entropy)?;
            let suffix = entropy
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let path = base.join(format!("oms-lifecycle-{suffix}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        fail("cannot allocate private lifecycle scratch directory")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn git(root: &Path, args: &[&str], flag: &AtomicBool) -> Result<String> {
    let program = find("git", root).ok_or_else(|| Error(2, "git is required".into()))?;
    let mut argv = vec![
        OsString::from("-c"),
        OsString::from("core.hooksPath=/dev/null"),
        OsString::from("-c"),
        OsString::from("core.fsmonitor=false"),
    ];
    argv.extend(args.iter().map(OsString::from));
    let output = command(program, &argv, root, None, flag, Duration::from_secs(120))?;
    if matches!(output.0, 130 | 124) {
        return Err(Error(
            output.0,
            "git command interrupted or timed out".into(),
        ));
    }
    if output.0 != 0 {
        return fail(format!(
            "git {} failed (exit {})",
            args.first().unwrap_or(&""),
            output.0
        ));
    }
    String::from_utf8(output.1)
        .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|_| Error(2, "git returned non UTF-8 metadata".into()))
}
/// Child output split on `\n`, `\r\n` and bare `\r`. Lines that still hold a
/// control character other than tab are dropped.
pub(crate) fn plain_lines(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.split(['\n', '\r'])
        .filter(|line| !line.chars().any(|c| c != '\t' && c.is_control()))
}
pub(crate) fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
}
pub(crate) fn shell(s: &OsStr) -> Result<String> {
    let s = s
        .to_str()
        .ok_or_else(|| Error(2, "configuration command path must be UTF-8".into()))?;
    if s.contains(['\n', '\r', '\0']) {
        return fail("configuration command path contains a separator");
    }
    if !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&c))
    {
        return Ok(s.to_owned());
    }
    Ok(format!("'{}'", s.replace('\'', "'\\''")))
}
pub(crate) fn native_host() -> Result<()> {
    fn windows(s: &str) -> bool {
        let s = s.to_ascii_lowercase();
        s == "windows"
            || s.starts_with("mingw")
            || s.starts_with("msys")
            || s.starts_with("cygwin")
            || s.starts_with("win32")
    }
    let actual = rustix::system::uname();
    if cfg!(windows)
        || windows(&actual.sysname().to_string_lossy())
        || ["OMS_PLATFORM_OVERRIDE", "OSTYPE"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .any(|s| windows(&s))
    {
        return Err(Error(
            1,
            "native Windows is not supported; install inside WSL2 on the Linux filesystem".into(),
        ));
    }
    Ok(())
}
pub(crate) fn child_guard() -> Result<()> {
    if std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|s| s == "1") {
        fail("a harness child cannot mutate OMS host lifecycle authority")
    } else {
        Ok(())
    }
}
pub(crate) fn new_file(path: &Path, raw: &[u8], mode: u32) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode & 0o777)
        .open(path)?;
    f.write_all(raw)?;
    f.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
    f.sync_all()?;
    Ok(())
}

pub(crate) struct ReceiptGuard {
    _operation: crate::lifecycle_lock::Guard,
    _receipt: Option<oms_lock::Guard>,
}
pub(crate) fn receipt_lock(
    paths: &crate::receipt::Paths,
    flag: &AtomicBool,
) -> Result<ReceiptGuard> {
    let operation = crate::lifecycle_lock::acquire(paths, flag)?;
    let receipt = receipt_only(paths)?;
    Ok(ReceiptGuard {
        _operation: operation,
        _receipt: receipt,
    })
}

pub(crate) fn receipt_only(paths: &crate::receipt::Paths) -> Result<Option<oms_lock::Guard>> {
    if crate::update::borrowed(paths)? {
        Ok(None)
    } else {
        lock(&paths.receipt, &paths.cwd).map(Some)
    }
}
