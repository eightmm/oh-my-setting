use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const LIMIT: usize = 16 * 1024 * 1024;
pub(crate) type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub(crate) struct Error {
    pub code: i32,
    pub message: String,
}
pub(crate) fn error(message: impl Into<String>) -> Error {
    Error {
        code: 2,
        message: message.into(),
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        error("filesystem or process operation failed")
    }
}
pub(crate) struct Output {
    pub code: i32,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
}
impl Output {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            code: 0,
            out: s.into().into_bytes(),
            err: Vec::new(),
        }
    }
    pub fn json(row: Value) -> Self {
        Self::text(format!("{row}\n"))
    }
}
pub(crate) struct Context<'a> {
    pub cwd: &'a Path,
    pub source: &'a Path,
    pub interrupted: &'a AtomicBool,
}
impl Context<'_> {
    pub fn check(&self) -> Result<()> {
        if self.interrupted.load(Ordering::Relaxed) {
            Err(Error {
                code: 130,
                message: "interrupted".into(),
            })
        } else {
            Ok(())
        }
    }
    pub fn path(&self, raw: &str) -> PathBuf {
        self.cwd.join(raw)
    }
    pub fn repo(&self, raw: Option<&str>) -> Result<PathBuf> {
        let path = self.path(raw.unwrap_or("."));
        let path = path.canonicalize()?;
        let out = self.command(
            "git",
            &[
                "-C",
                path.to_str()
                    .ok_or_else(|| error("non-UTF-8 repository path"))?,
                "rev-parse",
                "--show-toplevel",
            ],
            10,
        )?;
        if out.code == 0 {
            Ok(PathBuf::from(String::from_utf8_lossy(&out.out).trim()).canonicalize()?)
        } else {
            Ok(path)
        }
    }
    pub fn command(&self, bin: &str, args: &[&str], seconds: u64) -> Result<Output> {
        let mut command = Command::new(bin);
        command
            .args(args)
            .current_dir(self.cwd)
            .stdin(Stdio::null());
        capture(command, self.interrupted, seconds)
    }
}

struct ChildGroup {
    child: std::process::Child,
    group: rustix::process::Pid,
    reaped: bool,
}
impl ChildGroup {
    fn exited(&mut self) -> Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        loop {
            match waitid(
                WaitId::Pid(self.group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(status) => return Ok(status.is_some()),
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::CHILD) => {
                    self.reaped = true;
                    return Err(error("child identity lost; refusing group signal"));
                }
                Err(_) => return Err(error("cannot verify child process identity")),
            }
        }
    }
    fn finish(&mut self) -> Result<i32> {
        if self.reaped {
            return Err(error("child has already been reaped"));
        }
        self.exited()?;
        let _ = rustix::process::kill_process_group(self.group, rustix::process::Signal::KILL);
        self.reaped = true;
        Ok(self.child.wait()?.code().unwrap_or(1))
    }
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if !self.reaped && self.exited().is_ok() {
            let _ = self.finish();
        }
    }
}

/// Pipes are nonblocking; cancellation and deadlines cover the child process group.
pub(crate) fn capture(
    mut command: Command,
    interrupted: &AtomicBool,
    seconds: u64,
) -> Result<Output> {
    use rustix::process::Pid;
    use std::os::unix::process::CommandExt;
    command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Output {
                code: 127,
                out: vec![],
                err: vec![],
            });
        }
        Err(e) => return Err(e.into()),
    };
    let group = Pid::from_raw(child.id() as i32).ok_or_else(|| error("invalid child pid"))?;
    let mut group = ChildGroup {
        child,
        group,
        reaped: false,
    };
    let result = (|| {
        let mut stdout = group
            .child
            .stdout
            .take()
            .ok_or_else(|| error("missing child output"))?;
        let mut stderr = group
            .child
            .stderr
            .take()
            .ok_or_else(|| error("missing child diagnostics"))?;
        for fd in [
            &stdout as &dyn std::os::fd::AsFd,
            &stderr as &dyn std::os::fd::AsFd,
        ] {
            let flags =
                rustix::fs::fcntl_getfl(fd).map_err(|_| error("cannot inspect child pipe"))?;
            rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| error("cannot configure child pipe"))?;
        }
        let mut out = vec![];
        let mut err = vec![];
        let mut out_eof = false;
        let mut err_eof = false;
        let mut exit = None;
        let start = Instant::now();
        fn drain(input: &mut impl Read, saved: &mut Vec<u8>) -> Result<bool> {
            let mut chunk = [0u8; 8192];
            for _ in 0..128 {
                match input.read(&mut chunk) {
                    Ok(0) => return Ok(true),
                    Ok(n) => {
                        if saved.len().saturating_add(n) > LIMIT {
                            return Err(error("child output exceeded its byte budget"));
                        }
                        saved.extend_from_slice(&chunk[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(false)
        }
        loop {
            if interrupted.load(Ordering::Relaxed)
                || start.elapsed() >= Duration::from_secs(seconds)
            {
                if !group.reaped {
                    let _ = group.finish();
                }
                return Ok(Output {
                    code: if interrupted.load(Ordering::Relaxed) {
                        130
                    } else {
                        124
                    },
                    out,
                    err,
                });
            }
            if !out_eof {
                out_eof = drain(&mut stdout, &mut out)?;
            }
            if !err_eof {
                err_eof = drain(&mut stderr, &mut err)?;
            }
            if exit.is_none() && group.exited()? {
                exit = Some(group.finish()?);
            }
            if let Some(code) = exit
                && out_eof
                && err_eof
            {
                return Ok(Output { code, out, err });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    drop(group);
    result
}

pub(crate) struct Args {
    pub values: BTreeMap<String, Vec<String>>,
    pub flags: BTreeSet<String>,
    pub positional: Vec<String>,
}
impl Args {
    pub fn parse(args: &[OsString], values: &[&str], flags: &[&str]) -> Result<Self> {
        let mut out = Self {
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
            positional: vec![],
        };
        let mut args = args.iter();
        while let Some(raw) = args.next() {
            let raw = raw
                .to_str()
                .ok_or_else(|| error("arguments must be UTF-8"))?;
            if raw == "--" {
                for raw in args {
                    out.positional.push(
                        raw.to_str()
                            .ok_or_else(|| error("arguments must be UTF-8"))?
                            .into(),
                    );
                }
                break;
            }
            if values.contains(&raw) {
                let value = args
                    .next()
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| error(format!("{raw} requires a value")))?;
                out.values.entry(raw.into()).or_default().push(value.into());
            } else if flags.contains(&raw) {
                out.flags.insert(raw.into());
            } else if raw.starts_with('-') {
                return Err(error(format!("unknown argument: {raw}")));
            } else {
                out.positional.push(raw.into());
            }
        }
        Ok(out)
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
    pub fn all(&self, name: &str) -> impl Iterator<Item = &str> {
        self.values
            .get(name)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
    pub fn count(&self, maximum: usize) -> Result<()> {
        if self.positional.len() > maximum {
            Err(error("too many arguments"))
        } else {
            Ok(())
        }
    }
    pub fn required(&self, name: &str) -> Result<&str> {
        self.get(name)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error(format!("{name} is required")))
    }
}
pub(crate) fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| error("HOME must name an absolute directory"))
}
pub(crate) fn env_path(name: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}
pub(crate) fn host_mutation() -> Result<()> {
    if std::env::var("OMS_HARNESS_CHILD").as_deref() == Ok("1") {
        Err(error(
            "a harness child cannot mutate parent-owned host or global state",
        ))
    } else {
        Ok(())
    }
}
pub(crate) fn number(raw: &str, min: usize, max: usize, label: &str) -> Result<usize> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error(format!("{label} must be an integer")));
    }
    raw.parse::<usize>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| error(format!("{label} must be between {min} and {max}")))
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
pub(crate) fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| error("cannot encode JSON"))?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub(crate) fn json(raw: &[u8]) -> Result<Value> {
    oms_context::parse_json(raw).map_err(|_| error("invalid JSON"))?;
    serde_json::from_slice(raw).map_err(|_| error("invalid JSON"))
}
pub(crate) fn read(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let parent = path.parent().ok_or_else(|| error("missing parent"))?;
    let root = Root::open(parent).map_err(|_| error("cannot open file parent"))?;
    root.read_no_follow(
        Path::new(path.file_name().ok_or_else(|| error("missing filename"))?),
        ByteReadOptions {
            missing_ok: false,
            max_bytes: maximum,
        },
    )
    .map_err(|_| error("file is missing, unsafe, changed, or too large"))
}
pub(crate) fn optional(path: &Path) -> Result<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
        Ok(_) => read(path, LIMIT),
    }
}
pub(crate) fn lock(path: &Path) -> Result<oms_lock::Guard> {
    let settings = oms_lock::Settings::from_env().map_err(|_| error("invalid lock environment"))?;
    let lock = oms_lock::Lock::for_file(path, settings)
        .map_err(|_| error("cannot select transaction lock"))?;
    lock.acquire(None).map_err(|e| Error {
        code: e.exit_code(),
        message: "cannot acquire transaction lock".into(),
    })
}
pub(crate) fn write(path: &Path, bytes: &[u8], preserve: bool) -> Result<()> {
    let _guard = lock(path)?;
    write_locked(path, bytes, preserve, None)
}
pub(crate) fn write_locked(
    path: &Path,
    bytes: &[u8],
    preserve: bool,
    expected: Option<&[u8]>,
) -> Result<()> {
    let mut anchor = path
        .parent()
        .ok_or_else(|| error("missing destination parent"))?;
    while !anchor.is_dir() {
        anchor = anchor
            .parent()
            .ok_or_else(|| error("missing destination root"))?;
    }
    let root = Root::open(anchor).map_err(|_| error("cannot open destination root"))?;
    let store = Store { root };
    store.write_locked(path, bytes, preserve, expected)
}

/// Capture one physical repository root for descriptor-relative transactions.
pub(crate) struct Store {
    root: Root,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        Ok(Self {
            root: Root::open(root).map_err(|_| error("cannot open repository storage root"))?,
        })
    }
    pub fn write(&self, path: &Path, bytes: &[u8], preserve: bool) -> Result<()> {
        let _guard = lock(path)?;
        self.write_locked(path, bytes, preserve, None)
    }
    pub fn write_locked(
        &self,
        path: &Path,
        bytes: &[u8],
        preserve: bool,
        expected: Option<&[u8]>,
    ) -> Result<()> {
        self.root
            .atomic_mutate(
                path,
                MutationOptions {
                    read: ByteReadOptions {
                        missing_ok: true,
                        max_bytes: LIMIT,
                    },
                    create_parent: true,
                    preserve_existing: preserve,
                    ..Default::default()
                },
                |old| {
                    if expected.is_some_and(|value| value != old) {
                        return Err(error("destination changed during transaction"));
                    }
                    Ok(bytes.to_vec())
                },
            )
            .map_err(|e| error(format!("atomic write refused: {e:?}")))?;
        Ok(())
    }
    pub fn read(&self, path: &Path, maximum: usize) -> Result<Vec<u8>> {
        self.root
            .read_no_follow(
                path,
                ByteReadOptions {
                    missing_ok: false,
                    max_bytes: maximum,
                },
            )
            .map_err(|_| error("file is missing, unsafe, changed, or too large"))
    }
}

pub(crate) fn safe_relative(raw: &str) -> Result<&Path> {
    let path = Path::new(raw);
    if raw.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error("expected a relative path inside the repository"));
    }
    Ok(path)
}
pub(crate) fn no_links(root: &Path, relative: &Path) -> Result<()> {
    let mut walk = root.to_path_buf();
    for c in relative.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(error("invalid relative destination"));
        }
        walk.push(c);
        match std::fs::symlink_metadata(&walk) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err(error("symlink at destination; nothing was changed"));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(crate) fn clean(bytes: &[u8]) -> bool {
    oms_scrub::scan(bytes, &[]).is_ok_and(|hits| hits.is_empty())
}
pub(crate) fn installed(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if binary.contains('/') {
        return std::fs::metadata(binary)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| {
        std::fs::metadata(p.join(binary))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
pub(crate) fn walk(root: &Path, depth: usize, excluded: &[&str]) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, depth: usize, excluded: &[&str], out: &mut Vec<PathBuf>) -> Result<()> {
        if depth == 0 {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(root)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if excluded.iter().any(|n| entry.file_name() == *n) {
                continue;
            }
            if out.len() >= 10000 {
                return Err(error("directory scan exceeded the entry budget"));
            }
            let ty = entry.file_type()?;
            if ty.is_dir() {
                visit(&entry.path(), depth - 1, excluded, out)?;
            } else if ty.is_file() {
                out.push(entry.path());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, depth, excluded, &mut out)?;
    Ok(out)
}
