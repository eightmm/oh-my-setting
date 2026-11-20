use crate::value::V;
use crate::{Result, digest, random_hex};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use rustix::fs::{self as rfs, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(crate) fn expand(path: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let raw = path.as_os_str().as_bytes();
    if (raw == b"~" || raw.starts_with(b"~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut out = PathBuf::from(home);
        if raw.len() > 2 {
            out.push(std::ffi::OsStr::from_bytes(&raw[2..]));
        }
        out
    } else {
        path.to_path_buf()
    }
}

pub(crate) fn resolved(path: &Path) -> Result<PathBuf> {
    if let Ok(physical) = fs::canonicalize(path) {
        return Ok(physical);
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let parent = absolute.parent().ok_or("cannot resolve path")?;
    let name = absolute.file_name().ok_or("cannot resolve path")?;
    Ok(resolved(parent)?.join(name))
}

pub fn repo_root(raw: &Path, cwd: &Path) -> Result<PathBuf> {
    let path = expand(raw);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        use std::os::unix::ffi::OsStrExt;
        let raw = output.stdout.trim_ascii();
        if !raw.is_empty() {
            return fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(raw)))
                .map_err(|e| e.to_string());
        }
    }
    fs::canonicalize(&path)
        .map_err(|_| format!("repository path does not exist: {}", raw.display()))
}

pub(crate) fn resolve_read_repo(raw: &Path) -> Result<PathBuf> {
    fs::canonicalize(expand(raw))
        .map_err(|_| format!("repository path does not exist: {}", raw.display()))
}

pub fn git_head(repo: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| {
            (40..=64).contains(&s.len())
                && crate::projection::hex(&V::from(s.clone()), s.len(), false)
        })
        .unwrap_or_default()
}

pub fn event_path(repo: &Path) -> PathBuf {
    repo.join(".oms/lifecycle/events.jsonl")
}

pub fn approval_path(repo: &Path, create: bool) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(v) => expand(Path::new(&v)),
        None => home()?.join(".local/state"),
    };
    let directory = base.join("oh-my-setting/approvals");
    if create {
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
    }
    Ok(directory.join(format!(
        "{}.jsonl",
        digest(resolved(repo)?.as_os_str().as_bytes())
    )))
}

pub(crate) fn home() -> Result<PathBuf> {
    // This stdlib API also uses the native account database when HOME is
    // absent, matching pathlib rather than silently selecting the worktree.
    #[allow(deprecated)]
    let path = std::env::home_dir();
    path.ok_or_else(|| "home directory is unavailable".into())
}

pub(crate) fn ensure_oms(repo: &Path) -> Result<()> {
    let directory = repo.join(".oms");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    // Initialization precedes the lifecycle lock. Exclusive creation avoids
    // competing renames and never replaces an existing user's ignore file.
    match rfs::open(
        directory.join(".gitignore"),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    ) {
        Ok(fd) => {
            let mut file: File = fd.into();
            file.write_all(b"*\n")
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            File::open(&directory)
                .and_then(|parent| parent.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Err(rustix::io::Errno::EXIST) => (),
        Err(error) => return Err(error.to_string()),
    }
    fs::create_dir_all(directory.join("lifecycle")).map_err(|e| e.to_string())
}

fn same(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn regular(m: &fs::Metadata) -> bool {
    m.is_file() && m.nlink() == 1
}

/// The direct parent and named leaf stay bound to their opened identities.
/// Benign aliases above that parent are allowed by the canonical ledger API.
pub struct Ledger {
    path: PathBuf,
    parent: File,
    file: File,
    parent_before: fs::Metadata,
    opened: fs::Metadata,
    read_len: Option<u64>,
}
impl Ledger {
    pub fn open(path: &Path, write: bool, private: bool) -> Result<Self> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let parent_path = path.parent().ok_or("ledger has no parent")?;
        if write {
            fs::create_dir_all(parent_path).map_err(|e| e.to_string())?;
        }
        let before = fs::symlink_metadata(parent_path).map_err(|e| e.to_string())?;
        if !before.is_dir() {
            return Err("ledger parent must be a real directory".into());
        }
        let parent: File = rfs::open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| e.to_string())?
        .into();
        if !same(&before, &parent.metadata().map_err(|e| e.to_string())?) {
            return Err("ledger parent changed before open".into());
        }
        let old = match fs::symlink_metadata(&path) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        if old.as_ref().is_some_and(|m| !regular(m)) {
            return Err("ledger must be a regular non-linked file".into());
        }
        let mut flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        flags |= if write {
            OFlags::RDWR | OFlags::APPEND
        } else {
            OFlags::RDONLY
        };
        if write && old.is_none() {
            flags |= OFlags::CREATE | OFlags::EXCL;
        }
        let file: File = rfs::openat(
            &parent,
            path.file_name().ok_or("ledger has no name")?,
            flags,
            Mode::from_raw_mode(if private { 0o600 } else { 0o644 }),
        )
        .map_err(|e| e.to_string())?
        .into();
        let opened = file.metadata().map_err(|e| e.to_string())?;
        if !regular(&opened) || old.as_ref().is_some_and(|m| !same(m, &opened)) {
            return Err("ledger changed before open".into());
        }
        let ledger = Self {
            path,
            parent,
            file,
            parent_before: before,
            opened,
            read_len: None,
        };
        ledger.check()?;
        if private {
            ledger
                .file
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        Ok(ledger)
    }
    fn check(&self) -> Result<()> {
        let parent = fs::symlink_metadata(self.path.parent().ok_or("ledger has no parent")?)
            .map_err(|_| "ledger pathname disappeared during access")?;
        let named = fs::symlink_metadata(&self.path)
            .map_err(|_| "ledger pathname disappeared during access")?;
        let opened = self.file.metadata().map_err(|e| e.to_string())?;
        if !parent.is_dir()
            || !same(&parent, &self.parent_before)
            || !regular(&named)
            || !same(&named, &self.opened)
            || !regular(&opened)
        {
            return Err("ledger pathname changed during access".into());
        }
        Ok(())
    }
    pub fn read(&mut self) -> Result<Vec<u8>> {
        let bytes = self.read_from(0)?;
        self.read_len = Some(bytes.len() as u64);
        Ok(bytes)
    }
    fn read_from(&mut self, offset: u64) -> Result<Vec<u8>> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        self.file
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.check()?;
        Ok(bytes)
    }
    /// History was read through this descriptor while the caller held the ledger lock.
    pub(crate) fn append_batch(&mut self, rows: &mut [V], history: &[V]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.check()?;
        let length = self.file.metadata().map_err(|e| e.to_string())?.len();
        if self.read_len != Some(length) {
            return Err("ledger changed since locked snapshot".into());
        }
        let mut previous = std::collections::BTreeMap::new();
        let key = |row: &V| {
            let field = if row.get("attempt_id").truth() {
                "attempt_id"
            } else {
                "approval_id"
            };
            (field, crate::projection::identity(row.get(field)))
        };
        for row in history {
            previous.insert(key(row), row.clone());
        }
        let mut bytes = Vec::new();
        if length > 0 {
            self.file
                .seek(SeekFrom::End(-1))
                .map_err(|e| e.to_string())?;
            let mut last = [0];
            self.file.read_exact(&mut last).map_err(|e| e.to_string())?;
            if last[0] != b'\n' {
                bytes.push(b'\n');
            }
        }
        for row in rows {
            let identity = key(row);
            attach_trace(row, previous.get(&identity))?;
            bytes.extend_from_slice(&row.json(false)?);
            bytes.push(b'\n');
            previous.insert(identity, row.clone());
        }
        self.check()?;
        self.file
            .write_all(&bytes)
            .and_then(|_| self.file.sync_data())
            .map_err(|e| e.to_string())?;
        self.check()?;
        self.read_len = Some(length + bytes.len() as u64);
        self.parent.sync_all().map_err(|e| e.to_string())
    }
    pub fn append(&mut self, row: &mut V) -> Result<()> {
        let length = self
            .file
            .seek(SeekFrom::End(0))
            .map_err(|e| e.to_string())?;
        let start = length.saturating_sub(131072);
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut tail = Vec::new();
        (&mut self.file)
            .take(131072)
            .read_to_end(&mut tail)
            .map_err(|e| e.to_string())?;
        let identity = if row.get("attempt_id").truth() {
            "attempt_id"
        } else {
            "approval_id"
        };
        let mut lines = tail.split(|b| *b == b'\n').collect::<Vec<_>>();
        if start > 0 && !lines.is_empty() {
            lines.remove(0);
        }
        let previous = lines
            .into_iter()
            .rev()
            .filter_map(|line| V::parse(String::from_utf8_lossy(line).as_bytes()).ok())
            .find(|v| v.get(identity) == row.get(identity));
        attach_trace(row, previous.as_ref())?;
        let mut bytes = row.json(false)?;
        bytes.push(b'\n');
        if length > 0 && tail.last() != Some(&b'\n') {
            bytes.insert(0, b'\n');
        }
        self.check()?;
        self.file.write_all(&bytes).map_err(|e| e.to_string())?;
        self.file.sync_all().map_err(|e| e.to_string())?;
        self.check()?;
        self.parent.sync_all().map_err(|e| e.to_string())
    }
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && let Ok(m) = fs::symlink_metadata(parent)
                && !m.is_dir()
            {
                return Err("ledger parent must be a real directory".into());
            }
            Ok(Vec::new())
        }
        Err(e) => Err(e.to_string()),
        Ok(_) => Ledger::open(path, false, false)?.read(),
    }
}

pub(crate) fn normalized(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let text = String::from_utf8_lossy(bytes);
    if bytes.contains(&b'\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        text
    }
}

pub(crate) fn parse_rows_at(
    path: &Path,
    bytes: &[u8],
    line_offset: usize,
    interrupted: &AtomicBool,
) -> Result<(Vec<V>, usize)> {
    crate::checkpoint(interrupted)?;
    let text = normalized(bytes);
    let mut rows = Vec::new();
    let mut phys_lines = 0usize;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index > 0 && index % 256 == 0 {
            crate::checkpoint(interrupted)?;
        }
        phys_lines += 1;
        if line
            .chars()
            .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        {
            continue;
        }
        let line_no = line_offset + index + 1;
        let row = V::parse(line.as_bytes()).map_err(|_| {
            format!(
                "{}:{line_no} is not valid JSON: {}",
                path.display(),
                crate::diagnostic::json(line)
            )
        })?;
        if !matches!(row, V::Obj(_)) {
            return Err(format!("{}:{line_no} is not a JSON object", path.display()));
        }
        rows.push(row);
    }
    crate::checkpoint(interrupted)?;
    Ok((rows, phys_lines))
}

pub fn parse_rows(path: &Path, bytes: &[u8]) -> Result<Vec<V>> {
    parse_rows_at(path, bytes, 0, &AtomicBool::new(false)).map(|(rows, _)| rows)
}
pub fn read_rows(path: &Path) -> Result<Vec<V>> {
    parse_rows(path, &read_bytes(path)?)
}

/// Caller holds the events/approvals owner.json lock. A raw append is also
/// exposed for existing trace/telemetry consumers that already hold that lock.
pub fn append_row(path: &Path, row: &mut V, private: bool) -> Result<()> {
    Ledger::open(path, true, private)?.append(row)
}

pub(crate) fn atomic(path: &Path, bytes: &[u8], mode: u32, preserve: bool) -> Result<()> {
    let parent = path.parent().ok_or("atomic file has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let m = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if !m.is_dir() {
        return Err("ledger parent must be a real directory".into());
    }
    let root = Root::open(parent).map_err(|e| format!("atomic root: {e:?}"))?;
    let options = MutationOptions {
        read: ByteReadOptions {
            missing_ok: true,
            max_bytes: usize::MAX,
        },
        max_output: usize::MAX,
        preserve_existing: preserve,
        ..Default::default()
    };
    let outcome = root
        .atomic_mutate(
            Path::new(path.file_name().ok_or("atomic file has no name")?),
            options,
            |_| Ok::<_, String>(bytes.to_vec()),
        )
        .map_err(|e| format!("atomic write: {e:?}"))?;
    let opened = Ledger::open(path, false, false)?;
    if outcome.disposition != oms_safe_fs::MutationDisposition::Unchanged
        || opened.opened.permissions().mode() & 0o7777 != mode & 0o7777
    {
        opened
            .file
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    opened.check()?;
    opened.file.sync_all().map_err(|e| e.to_string())?;
    opened.parent.sync_all().map_err(|e| e.to_string())
}
pub fn write_json_atomic(path: &Path, value: &V, mode: u32) -> Result<()> {
    let mut bytes = value.json(true)?;
    bytes.push(b'\n');
    atomic(path, &bytes, mode, false)
}

pub(crate) fn trace_valid(row: &V) -> bool {
    use crate::projection::hex;
    hex(row.get("trace_id"), 32, true)
        && hex(row.get("span_id"), 16, true)
        && hex(&row.default("trace_flags", "00"), 2, false)
        && (row.get("parent_span_id") == &V::Null || hex(row.get("parent_span_id"), 16, true))
}
fn attach_trace(row: &mut V, previous: Option<&V>) -> Result<()> {
    let inherited = previous.filter(|v| trace_valid(v));
    let fields = if let Some(previous) = inherited {
        Some((
            previous.get("trace_id").clone(),
            previous.get("span_id").clone(),
            previous.default("trace_flags", "00"),
        ))
    } else {
        let raw = std::env::var("OMS_TRACEPARENT").unwrap_or_default();
        let parts: Vec<_> = raw.split('-').collect();
        if parts.len() == 4
            && parts[0] != "ff"
            && crate::projection::hex(&V::from(parts[0]), 2, false)
            && crate::projection::hex(&V::from(parts[1]), 32, true)
            && crate::projection::hex(&V::from(parts[2]), 16, true)
            && crate::projection::hex(&V::from(parts[3]), 2, false)
        {
            Some((parts[1].into(), parts[2].into(), parts[3].into()))
        } else {
            None
        }
    };
    if let Some((trace, parent, flags)) = fields {
        row.set("trace_id", trace);
        row.set("span_id", random_hex(8)?);
        row.set("parent_span_id", parent);
        row.set("trace_flags", flags);
    }
    Ok(())
}

pub(crate) fn stat_identity(path: &Path) -> Result<(u64, u64)> {
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_dir() {
        return Err("lock must be a real directory".into());
    }
    Ok((m.dev(), m.ino()))
}

#[derive(Default)]
pub(crate) struct FollowTail {
    identity: Option<(u64, u64)>,
    offset: u64,
}
impl FollowTail {
    pub(crate) fn read(&mut self, path: &Path) -> Result<(bool, Vec<(V, oms_context::Json)>)> {
        let mut ledger = match Ledger::open(path, false, false) {
            Ok(ledger) => ledger,
            Err(_)
                if fs::symlink_metadata(path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                read_bytes(path)?;
                let reset = self.identity.take().is_some();
                self.offset = 0;
                return Ok((reset, Vec::new()));
            }
            Err(e) => return Err(e),
        };
        let metadata = ledger.file.metadata().map_err(|e| e.to_string())?;
        let identity = (metadata.dev(), metadata.ino());
        let reset = self.identity != Some(identity) || metadata.len() < self.offset;
        let offset = if reset { 0 } else { self.offset };
        let bytes = ledger.read_from(offset)?;
        let rows = follow_bytes(path, &bytes)?;
        self.identity = Some(identity);
        self.offset = offset + bytes.len() as u64;
        Ok((reset, rows))
    }
}

pub(crate) fn follow_bytes(path: &Path, bytes: &[u8]) -> Result<Vec<(V, oms_context::Json)>> {
    let rows = parse_rows(path, bytes)?;
    let text = normalized(bytes);
    rows.into_iter()
        .zip(text.split_inclusive('\n').filter(|line| {
            !line
                .chars()
                .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        }))
        .map(|(row, raw)| {
            let ordered = oms_context::parse_experiment_json(raw.as_bytes(), usize::MAX)
                .map_err(|_| "invalid JSON")?;
            Ok((row, ordered))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerPrefixMeta {
    pub(crate) repo_id: (u64, u64),
    pub(crate) oms_id: (u64, u64),
    pub(crate) parent_id: (u64, u64),
    pub(crate) file_id: (u64, u64),
    pub(crate) offset: u64,
    pub(crate) prefix_sha256: [u8; 32],
    pub(crate) unterminated: bool,
}

pub(crate) enum EventsReadSnapshot {
    Absent {
        canonical_repo: PathBuf,
    },
    Incremental {
        canonical_repo: PathBuf,
        path: PathBuf,
        meta: LedgerPrefixMeta,
        delta: Vec<u8>,
    },
    Full {
        canonical_repo: PathBuf,
        path: PathBuf,
        meta: LedgerPrefixMeta,
        bytes: Vec<u8>,
    },
}

struct AncestorIds {
    repo: (u64, u64),
    oms: (u64, u64),
    lifecycle: (u64, u64),
}

fn file_stamp(m: &fs::Metadata) -> (i64, i64, i64, i64) {
    (m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec())
}

fn map_dir_open_err(e: rustix::io::Errno) -> String {
    match e {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
            "ledger parent must be a real directory".into()
        }
        _ => e.to_string(),
    }
}

fn verify_dir_identity(path: &Path, expected: (u64, u64)) -> Result<()> {
    let m = fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            "ledger pathname disappeared during access".to_string()
        } else {
            err.to_string()
        }
    })?;
    if !m.is_dir() {
        return Err("ledger parent must be a real directory".into());
    }
    if (m.dev(), m.ino()) != expected {
        return Err("ledger pathname changed during access".into());
    }
    Ok(())
}

fn verify_ancestors(
    canonical_repo: &Path,
    repo_id: (u64, u64),
    oms_id: Option<(u64, u64)>,
    lifecycle_id: Option<(u64, u64)>,
) -> Result<()> {
    verify_dir_identity(canonical_repo, repo_id)?;
    if let Some(oms) = oms_id {
        verify_dir_identity(&canonical_repo.join(".oms"), oms)?;
    }
    if let Some(lifecycle) = lifecycle_id {
        verify_dir_identity(&canonical_repo.join(".oms/lifecycle"), lifecycle)?;
    }
    Ok(())
}

fn hash_prefix(
    file: &mut File,
    buf: &mut [u8],
    prefix_len: u64,
    hasher: &mut Sha256,
    interrupted: &AtomicBool,
) -> Result<bool> {
    let mut remaining = prefix_len;
    while remaining > 0 {
        crate::checkpoint(interrupted)?;
        let to_read = remaining.min(buf.len() as u64) as usize;
        match file.read(&mut buf[..to_read]) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                hasher.update(&buf[..n]);
                remaining -= n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(true)
}

const INITIAL_READ_CAP: u64 = 65_536;

pub(crate) fn new_read_vec(expected_remaining: u64) -> Result<Vec<u8>> {
    isize::try_from(expected_remaining)
        .map_err(|_| "ledger exceeds addressable memory".to_string())?;
    let initial = expected_remaining.min(INITIAL_READ_CAP) as usize;
    let mut out = Vec::new();
    out.try_reserve(initial)
        .map_err(|_| "ledger exceeds available memory".to_string())?;
    Ok(out)
}

pub(crate) fn read_into(
    file: &mut File,
    buf: &mut [u8],
    expected_remaining: u64,
    hasher: &mut Sha256,
    out: &mut Vec<u8>,
    interrupted: &AtomicBool,
) -> Result<bool> {
    let mut remaining = expected_remaining;
    while remaining > 0 {
        crate::checkpoint(interrupted)?;
        let to_read = remaining.min(buf.len() as u64) as usize;
        if to_read == 0 {
            return Err("read buffer must not be empty".into());
        }
        match file.read(&mut buf[..to_read]) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                hasher.update(&buf[..n]);
                out.try_reserve(n)
                    .map_err(|_| "ledger exceeds available memory".to_string())?;
                out.extend_from_slice(&buf[..n]);
                remaining -= n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    loop {
        crate::checkpoint(interrupted)?;
        let mut probe = [0u8; 1];
        match file.read(&mut probe) {
            Ok(0) => return Ok(true),
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn verify_read_stable(
    ledger: &Ledger,
    canonical_repo: &Path,
    ancestors: &AncestorIds,
    expected_len: u64,
    actual_len: u64,
    stamp0: (i64, i64, i64, i64),
) -> Result<bool> {
    ledger.check()?;
    verify_ancestors(
        canonical_repo,
        ancestors.repo,
        Some(ancestors.oms),
        Some(ancestors.lifecycle),
    )?;
    let m1 = ledger.file.metadata().map_err(|e| e.to_string())?;
    Ok(actual_len == expected_len && m1.len() == expected_len && file_stamp(&m1) == stamp0)
}

fn open_verified_child_dir(
    parent_fd: &File,
    child_path: &Path,
    name: &str,
    dir_flags: OFlags,
) -> Result<Option<(File, (u64, u64))>> {
    let file: File = match rfs::openat(parent_fd, name, dir_flags, Mode::empty()) {
        Ok(fd) => fd.into(),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(e) => return Err(map_dir_open_err(e)),
    };
    let meta = fs::symlink_metadata(child_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "ledger pathname disappeared during access".to_string()
        } else {
            e.to_string()
        }
    })?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("ledger parent must be a real directory".into());
    }
    if !same(&meta, &opened) {
        return Err("ledger pathname changed during access".into());
    }
    Ok(Some((file, (opened.dev(), opened.ino()))))
}

pub(crate) fn read_events_snapshot(
    canonical_repo: PathBuf,
    prev: Option<&LedgerPrefixMeta>,
    interrupted: &AtomicBool,
) -> Result<EventsReadSnapshot> {
    const CHUNK_SIZE: usize = 65_536;

    crate::checkpoint(interrupted)?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;

    let repo_before = fs::symlink_metadata(&canonical_repo).map_err(|e| e.to_string())?;
    if !repo_before.is_dir() {
        return Err("ledger parent must be a real directory".into());
    }
    let repo_file: File = rfs::open(&canonical_repo, dir_flags, Mode::empty())
        .map_err(map_dir_open_err)?
        .into();
    let repo_opened = repo_file.metadata().map_err(|e| e.to_string())?;
    if !repo_opened.is_dir() || !same(&repo_before, &repo_opened) {
        return Err("ledger pathname changed during access".into());
    }
    let repo_id = (repo_opened.dev(), repo_opened.ino());

    let Some((oms_file, oms_id)) =
        open_verified_child_dir(&repo_file, &canonical_repo.join(".oms"), ".oms", dir_flags)?
    else {
        verify_ancestors(&canonical_repo, repo_id, None, None)?;
        return Ok(EventsReadSnapshot::Absent { canonical_repo });
    };

    let Some((_lifecycle_file, lifecycle_id)) = open_verified_child_dir(
        &oms_file,
        &canonical_repo.join(".oms/lifecycle"),
        "lifecycle",
        dir_flags,
    )?
    else {
        verify_ancestors(&canonical_repo, repo_id, Some(oms_id), None)?;
        return Ok(EventsReadSnapshot::Absent { canonical_repo });
    };
    verify_ancestors(&canonical_repo, repo_id, Some(oms_id), Some(lifecycle_id))?;

    let path = event_path(&canonical_repo);
    let mut ledger = match Ledger::open(&path, false, false) {
        Ok(ledger) => ledger,
        Err(e) => {
            if fs::symlink_metadata(&path)
                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
            {
                verify_ancestors(&canonical_repo, repo_id, Some(oms_id), Some(lifecycle_id))?;
                return Ok(EventsReadSnapshot::Absent { canonical_repo });
            }
            return Err(e);
        }
    };
    if (ledger.parent_before.dev(), ledger.parent_before.ino()) != lifecycle_id {
        return Err("ledger pathname changed during access".into());
    }
    verify_ancestors(&canonical_repo, repo_id, Some(oms_id), Some(lifecycle_id))?;

    let ancestors = AncestorIds {
        repo: repo_id,
        oms: oms_id,
        lifecycle: lifecycle_id,
    };
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut allow_incremental = prev.is_some();
    let max_attempts = if allow_incremental { 4 } else { 3 };

    for _ in 0..max_attempts {
        crate::checkpoint(interrupted)?;
        ledger
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let m0 = ledger.file.metadata().map_err(|e| e.to_string())?;
        if !regular(&m0) {
            return Err("ledger must be a regular non-linked file".into());
        }
        let expected_len = m0.len();
        let file_id = (m0.dev(), m0.ino());
        let stamp0 = file_stamp(&m0);

        if allow_incremental
            && let Some(p) = prev.filter(|p| {
                p.repo_id == ancestors.repo
                    && p.oms_id == ancestors.oms
                    && p.parent_id == ancestors.lifecycle
                    && p.file_id == file_id
                    && !p.unterminated
                    && expected_len >= p.offset
            })
        {
            let mut hasher = Sha256::new();
            if !hash_prefix(
                &mut ledger.file,
                &mut buf,
                p.offset,
                &mut hasher,
                interrupted,
            )? {
                continue;
            }
            let prefix_digest: [u8; 32] = hasher.clone().finalize().into();
            if prefix_digest != p.prefix_sha256 {
                allow_incremental = false;
                continue;
            }
            let delta_len = expected_len - p.offset;
            let mut delta = new_read_vec(delta_len)?;
            if !read_into(
                &mut ledger.file,
                &mut buf,
                delta_len,
                &mut hasher,
                &mut delta,
                interrupted,
            )? {
                continue;
            }
            let actual_len = p.offset.saturating_add(delta.len() as u64);
            if !verify_read_stable(
                &ledger,
                &canonical_repo,
                &ancestors,
                expected_len,
                actual_len,
                stamp0,
            )? {
                continue;
            }
            let unterminated = !delta.is_empty() && delta.last() != Some(&b'\n');
            return Ok(EventsReadSnapshot::Incremental {
                canonical_repo,
                path,
                meta: LedgerPrefixMeta {
                    repo_id: ancestors.repo,
                    oms_id: ancestors.oms,
                    parent_id: ancestors.lifecycle,
                    file_id,
                    offset: expected_len,
                    prefix_sha256: hasher.finalize().into(),
                    unterminated,
                },
                delta,
            });
        }

        // Cold Full Read: no prior entry, metadata/inode mismatch, shrunken, unterminated, or prefix mismatch.
        let mut hasher = Sha256::new();
        let mut bytes = new_read_vec(expected_len)?;
        if !read_into(
            &mut ledger.file,
            &mut buf,
            expected_len,
            &mut hasher,
            &mut bytes,
            interrupted,
        )? {
            continue;
        }
        if !verify_read_stable(
            &ledger,
            &canonical_repo,
            &ancestors,
            expected_len,
            bytes.len() as u64,
            stamp0,
        )? {
            continue;
        }
        let unterminated = !bytes.is_empty() && bytes.last() != Some(&b'\n');
        return Ok(EventsReadSnapshot::Full {
            canonical_repo,
            path,
            meta: LedgerPrefixMeta {
                repo_id: ancestors.repo,
                oms_id: ancestors.oms,
                parent_id: ancestors.lifecycle,
                file_id,
                offset: expected_len,
                prefix_sha256: hasher.finalize().into(),
                unterminated,
            },
            bytes,
        });
    }

    Err("ledger changed during access".into())
}
