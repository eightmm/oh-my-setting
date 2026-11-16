use crate::value::V;
use crate::{Result, digest, random_hex};
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use rustix::fs::{self as rfs, Mode, OFlags};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

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

pub fn parse_rows(path: &Path, bytes: &[u8]) -> Result<Vec<V>> {
    let text = normalized(bytes);
    let mut rows = Vec::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if line
            .chars()
            .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        {
            continue;
        }
        let row = V::parse(line.as_bytes()).map_err(|_| {
            format!(
                "{}:{} is not valid JSON: {}",
                path.display(),
                index + 1,
                crate::diagnostic::json(line)
            )
        })?;
        if !matches!(row, V::Obj(_)) {
            return Err(format!(
                "{}:{} is not a JSON object",
                path.display(),
                index + 1
            ));
        }
        rows.push(row);
    }
    Ok(rows)
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
