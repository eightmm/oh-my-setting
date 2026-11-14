//! Private canonical runtime files, anchored by retained directory descriptors.
use crate::Result;
use oms_command_operations::value::V;
use rustix::fs::{self as fs, AtFlags, FileType, Mode, OFlags, Stat};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

const RECORD_LIMIT: usize = 16 * 1024 * 1024;
fn err(e: impl std::fmt::Display) -> String {
    format!("unsafe or unavailable supervisor store: {e}")
}
fn same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn generation(a: &Stat, b: &Stat) -> bool {
    same(a, b)
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
        && a.st_nlink == b.st_nlink
}
fn regular(s: &Stat) -> Result<()> {
    if FileType::from_raw_mode(s.st_mode) != FileType::RegularFile
        || s.st_nlink != 1
        || s.st_uid != rustix::process::getuid().as_raw()
        || s.st_mode & 0o077 != 0
    {
        Err("unsafe private supervisor file".into())
    } else {
        Ok(())
    }
}
fn leaf(name: &OsStr) -> Result<()> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        Err("invalid supervisor filename".into())
    } else {
        Ok(())
    }
}
struct Anchor {
    parent: OwnedFd,
    name: OsString,
    stat: Stat,
    system_link: Option<Stat>,
}
pub(crate) struct Directory {
    pub path: PathBuf,
    fd: OwnedFd,
    chain: Vec<Anchor>,
}
impl Directory {
    fn open(path: &Path, create: bool, private_from: usize) -> Result<Self> {
        if !path.is_absolute() {
            return Err("supervisor state root must be absolute".into());
        }
        let mut fd = fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(err)?;
        let mut chain = Vec::new();
        for (index, component) in path
            .components()
            .filter(|c| *c != Component::RootDir)
            .enumerate()
        {
            let Component::Normal(name) = component else {
                return Err("invalid supervisor state ancestor".into());
            };
            let mut created = false;
            if create {
                match fs::mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) => created = true,
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(e) => return Err(err(e)),
                }
            }
            // macOS system aliases such as /tmp and /var are root-owned.
            // Never follow a user-controlled link or a link inside state home.
            let named = fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(err)?;
            let parent = fs::fstat(&fd).map_err(err)?;
            let system_link = (FileType::from_raw_mode(named.st_mode) == FileType::Symlink
                && index + 1 < private_from
                && named.st_uid == 0
                && parent.st_uid == 0
                && parent.st_mode & 0o022 == 0)
                .then_some(named);
            let nofollow = if system_link.is_some() {
                OFlags::empty()
            } else {
                OFlags::NOFOLLOW
            };
            let child = fs::openat(
                &fd,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | nofollow | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(err)?;
            if created {
                fs::fchmod(&child, Mode::from_raw_mode(0o700)).map_err(err)?;
                fs::fsync(&child).map_err(err)?;
                fs::fsync(&fd).map_err(err)?;
            }
            let stat = fs::fstat(&child).map_err(err)?;
            let private = index >= private_from;
            if index + 1 == private_from
                && (stat.st_uid != rustix::process::getuid().as_raw() || stat.st_mode & 0o022 != 0)
            {
                return Err(
                    "supervisor state home must be owned and not writable by others".into(),
                );
            }
            let sticky_root = stat.st_uid == 0 && stat.st_mode & 0o1000 != 0;
            if stat.st_mode & 0o022 != 0 && !sticky_root
                || private
                    && (stat.st_uid != rustix::process::getuid().as_raw()
                        || stat.st_mode & 0o077 != 0)
            {
                return Err("unsafe supervisor state directory".into());
            }
            chain.push(Anchor {
                parent: fd,
                name: name.to_os_string(),
                stat,
                system_link,
            });
            fd = child;
        }
        let directory = Self {
            path: path.to_path_buf(),
            fd,
            chain,
        };
        directory.check()?;
        Ok(directory)
    }
    pub fn check(&self) -> Result<()> {
        for anchor in &self.chain {
            let named =
                fs::statat(&anchor.parent, &anchor.name, AtFlags::SYMLINK_NOFOLLOW).map_err(err)?;
            let current = if let Some(link) = &anchor.system_link {
                if !generation(&named, link)
                    || named.st_uid != 0
                    || FileType::from_raw_mode(named.st_mode) != FileType::Symlink
                {
                    return Err("supervisor system ancestor changed".into());
                }
                fs::statat(&anchor.parent, &anchor.name, AtFlags::empty()).map_err(err)?
            } else {
                named
            };
            if !same(&current, &anchor.stat)
                || FileType::from_raw_mode(current.st_mode) != FileType::Directory
                || current.st_mode != anchor.stat.st_mode
                || current.st_uid != anchor.stat.st_uid
            {
                return Err("supervisor namespace changed".into());
            }
        }
        if let Some(last) = self.chain.last()
            && !same(&fs::fstat(&self.fd).map_err(err)?, &last.stat)
        {
            return Err("supervisor root changed".into());
        }
        Ok(())
    }
    fn observed(&self, name: &OsStr) -> Result<Option<Stat>> {
        self.check()?;
        leaf(name)?;
        match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => {
                regular(&s)?;
                Ok(Some(s))
            }
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }
    fn unchanged(&self, name: &OsStr, expected: Option<&Stat>) -> Result<()> {
        let now = self.observed(name)?;
        if match (expected, now.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => generation(a, b),
            _ => false,
        } {
            Ok(())
        } else {
            Err("supervisor file changed during operation".into())
        }
    }
    fn read(&self, name: &OsStr, limit: usize) -> Result<Option<Vec<u8>>> {
        self.read_snapshot(name, limit, false)
    }
    fn read_snapshot(&self, name: &OsStr, limit: usize, growing: bool) -> Result<Option<Vec<u8>>> {
        let Some(before) = self.observed(name)? else {
            return Ok(None);
        };
        if before.st_size < 0 || before.st_size as u64 > limit as u64 {
            return Err("supervisor record exceeds safe read bound".into());
        }
        let fd = fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(err)?;
        let opened = fs::fstat(&fd).map_err(err)?;
        regular(&opened)?;
        if !(if growing {
            same(&before, &opened) && opened.st_size >= before.st_size
        } else {
            generation(&before, &opened)
        }) {
            return Err("supervisor record changed before read".into());
        }
        if opened.st_size < 0 || opened.st_size as u64 > limit as u64 {
            return Err("supervisor record exceeds safe read bound".into());
        }
        let mut file = File::from(fd);
        let mut data = Vec::new();
        (&mut file)
            .take(if growing {
                opened.st_size as u64
            } else {
                limit as u64 + 1
            })
            .read_to_end(&mut data)
            .map_err(err)?;
        if growing {
            let after = fs::fstat(&file).map_err(err)?;
            regular(&after)?;
            let named = self.observed(name)?.ok_or("supervisor log disappeared")?;
            if data.len() as u64 != opened.st_size as u64
                || !same(&opened, &after)
                || !same(&opened, &named)
                || after.st_size < opened.st_size
                || named.st_size < opened.st_size
            {
                return Err("supervisor log replaced or shortened while reading".into());
            }
            return Ok(Some(data));
        }
        if data.len() > limit
            || data.len() as i64 != before.st_size
            || !generation(&before, &fs::fstat(&file).map_err(err)?)
        {
            return Err("supervisor record changed while reading".into());
        }
        self.unchanged(name, Some(&before))?;
        Ok(Some(data))
    }
    fn write(&self, name: &OsStr, bytes: &[u8]) -> Result<()> {
        let before = self.observed(name)?;
        let temporary = OsString::from(format!("{}.{}", name.to_string_lossy(), nonce()?));
        let fd = fs::openat(
            &self.fd,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(err)?;
        let mut file = File::from(fd);
        let created = fs::fstat(&file).map_err(err)?;
        let result = (|| {
            fs::fchmod(&file, Mode::from_raw_mode(0o600)).map_err(err)?;
            file.write_all(bytes).map_err(err)?;
            file.sync_all().map_err(err)?;
            self.unchanged(name, before.as_ref())?;
            let named = fs::statat(&self.fd, &temporary, AtFlags::SYMLINK_NOFOLLOW).map_err(err)?;
            regular(&named)?;
            if !same(&created, &named) {
                return Err("supervisor temporary file changed".into());
            }
            fs::renameat(&self.fd, &temporary, &self.fd, name)
                .map_err(|e| format!("supervisor publication outcome uncertain: {e}"))?;
            fs::fsync(&self.fd).map_err(|e| {
                format!("supervisor publication visible but durability uncertain: {e}")
            })?;
            self.check()
        })();
        if let Ok(named) = fs::statat(&self.fd, &temporary, AtFlags::SYMLINK_NOFOLLOW)
            && same(&created, &named)
            && regular(&named).is_ok()
        {
            let _ = fs::unlinkat(&self.fd, &temporary, AtFlags::empty());
        }
        result
    }
    fn names(&self) -> Result<Vec<OsString>> {
        self.check()?;
        let mut dir = fs::Dir::read_from(&self.fd).map_err(err)?;
        let mut names = Vec::new();
        for item in &mut dir {
            let item = item.map_err(err)?;
            let name = OsStr::from_bytes(item.file_name().to_bytes());
            if name != "." && name != ".." {
                names.push(name.to_owned());
            }
        }
        self.check()?;
        names.sort();
        Ok(names)
    }
    fn remove(&self, name: &OsStr) -> Result<()> {
        let Some(before) = self.observed(name)? else {
            return Ok(());
        };
        self.unchanged(name, Some(&before))?;
        fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(err)?;
        fs::fsync(&self.fd).map_err(err)?;
        self.check()
    }
}
pub(crate) struct Store {
    pub root: Directory,
    pub state_home: PathBuf,
    jobs: Directory,
    logs: Directory,
}
impl Store {
    pub fn open(repo: &Path, default_repo: &Path) -> Result<Self> {
        let base = match std::env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
            Some(s) => expand(Path::new(&s))?,
            None => home()?.join(".local/state"),
        };
        let base = if base.is_absolute() {
            base
        } else {
            default_repo.join(base)
        };
        let mut base_normal = PathBuf::from("/");
        for component in base.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(c) => base_normal.push(c),
                Component::ParentDir => {
                    if !base_normal.pop() {
                        return Err("invalid state path".into());
                    }
                }
                _ => return Err("invalid state path".into()),
            }
        }
        let private_from = base_normal
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .count();
        let digest = digest(repo.as_os_str().as_bytes());
        let path = base_normal.join("oh-my-setting/runtime").join(digest);
        let root = Directory::open(&path, true, private_from)?;
        let jobs = Directory::open(&path.join("jobs"), true, private_from)?;
        let logs = Directory::open(&path.join("logs"), true, private_from)?;
        Ok(Self {
            root,
            jobs,
            logs,
            state_home: base_normal,
        })
    }
    pub fn sync_jobs(&self) -> Result<()> {
        self.root.check()?;
        self.jobs.check()?;
        fs::fsync(&self.jobs.fd)
            .map_err(|e| format!("supervisor job visible but durability uncertain: {e}"))?;
        self.jobs.check()
    }
    pub fn queue_path(&self) -> PathBuf {
        self.root.path.join("queue.state")
    }
    pub fn load(&self, id: &str) -> Result<V> {
        valid_id(id)?;
        let raw = self
            .jobs
            .read(OsStr::new(&format!("{id}.json")), RECORD_LIMIT)?
            .ok_or_else(|| format!("no supervised job for attempt: {id}"))?;
        let row = V::parse(&raw)?;
        if !matches!(row, V::Obj(_))
            || row.get("schema") != &V::from(1usize)
            || row.get("attempt_id").text() != Some(id)
            || row.get("runtime_state").text().is_none()
        {
            return Err(format!("invalid supervised job: {id}"));
        }
        if ![
            "queued",
            "launching",
            "starting",
            "running",
            "cancel_requested",
            "cleanup_failed",
            "finished",
            "cancelled",
            "abandoned",
            "interrupted",
        ]
        .contains(&row.get("runtime_state").text().unwrap_or(""))
        {
            return Err("unknown supervised runtime state".into());
        }
        for key in [
            "pid",
            "runner_pid",
            "log_bytes",
            "max_log_bytes",
            "reserved_tokens",
            "reserved_cost_microusd",
            "queue_order",
        ] {
            let in_range = if key == "queue_order" {
                row.get(key).py().parse::<i128>().is_ok()
            } else {
                row.get(key).py().parse::<i64>().is_ok()
            };
            if row.get(key) != &V::Null && (!row.get(key).integer(false) || !in_range) {
                return Err(format!("invalid supervised job integer: {key}"));
            }
        }
        Ok(row)
    }
    pub fn save(&self, row: &mut V) -> Result<()> {
        let id = row
            .get("attempt_id")
            .text()
            .ok_or("job has no attempt id")?
            .to_owned();
        valid_id(&id)?;
        row.set("updated_at", now());
        let data = row.json(true)?;
        if data.len() > RECORD_LIMIT {
            return Err("supervisor record exceeds safe read bound".into());
        }
        self.jobs.write(OsStr::new(&format!("{id}.json")), &data)
    }
    pub fn exists(&self, id: &str) -> Result<bool> {
        valid_id(id)?;
        Ok(self
            .jobs
            .observed(OsStr::new(&format!("{id}.json")))?
            .is_some())
    }
    pub fn all(&self, strict: bool) -> Result<Vec<V>> {
        let mut jobs = Vec::new();
        for name in self.jobs.names()? {
            let bytes = name.as_bytes();
            if !bytes.starts_with(b"att_") || !bytes.ends_with(b".json") {
                continue;
            }
            let id = std::str::from_utf8(&bytes[..bytes.len() - 5]).map_err(err)?;
            match self.load(id) {
                Ok(row) => jobs.push(row),
                Err(_) if !strict => {}
                Err(_) => {
                    return Err(format!(
                        "cannot safely read runtime job {}; run reconcile after repairing the record",
                        name.to_string_lossy()
                    ));
                }
            }
        }
        Ok(jobs)
    }
    pub fn read_log(&self, id: &str) -> Result<Vec<u8>> {
        valid_id(id)?;
        Ok(self
            .logs
            .read_snapshot(OsStr::new(&format!("{id}.log")), 100 * 1024 * 1024, true)?
            .unwrap_or_default())
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        valid_id(id)?;
        self.logs.remove(OsStr::new(&format!("{id}.log")))?;
        self.jobs.remove(OsStr::new(&format!("{id}.json")))
    }
    pub fn log(&self, id: &str, limit: usize) -> Result<Log> {
        valid_id(id)?;
        let name = OsString::from(format!("{id}.log"));
        let before = self.logs.observed(&name)?;
        let fd = fs::openat(
            &self.logs.fd,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(err)?;
        let stat = fs::fstat(&fd).map_err(err)?;
        regular(&stat)?;
        if let Some(before) = before
            && !generation(&before, &stat)
        {
            return Err("log changed before opening".into());
        }
        fs::fchmod(&fd, Mode::from_raw_mode(0o600)).map_err(err)?;
        fs::ftruncate(&fd, 0).map_err(err)?;
        fs::fsync(&self.logs.fd).map_err(err)?;
        Ok(Log {
            file: File::from(fd),
            name,
            identity: stat,
            written: 0,
            limit,
        })
    }
    pub fn append_log(&self, log: &mut Log, prefix: &[u8]) -> Result<()> {
        if prefix.len() <= log.written {
            return Ok(());
        }
        self.logs.check()?;
        let named = self.logs.observed(&log.name)?.ok_or("log disappeared")?;
        if !same(&named, &log.identity) {
            return Err("log identity changed".into());
        }
        if prefix.len() > log.limit {
            return Err("bounded log overflow".into());
        }
        log.file.write_all(&prefix[log.written..]).map_err(err)?;
        log.written = prefix.len();
        Ok(())
    }
    pub fn finish_log(&self, log: &mut Log, bytes: &[u8]) -> Result<()> {
        if bytes.len() > log.limit {
            return Err("bounded log overflow".into());
        }
        self.logs.check()?;
        let named = self.logs.observed(&log.name)?.ok_or("log disappeared")?;
        if !same(&named, &log.identity) {
            return Err("log identity changed".into());
        }
        log.file.seek(SeekFrom::Start(0)).map_err(err)?;
        log.file.write_all(bytes).map_err(err)?;
        log.file.set_len(bytes.len() as u64).map_err(err)?;
        log.file.sync_all().map_err(err)?;
        log.written = bytes.len();
        self.logs.check()
    }
}
pub(crate) struct Log {
    file: File,
    name: OsString,
    identity: Stat,
    written: usize,
    limit: usize,
}
pub(crate) fn valid_id(raw: &str) -> Result<()> {
    if oms_command_operations::projection::id(raw) {
        Ok(())
    } else {
        Err("attempt must match [A-Za-z0-9][A-Za-z0-9._:-]{0,159}".into())
    }
}
pub(crate) fn expand(path: &Path) -> Result<PathBuf> {
    let raw = path.as_os_str().as_bytes();
    if raw == b"~" || raw.starts_with(b"~/") {
        let home = home()?;
        Ok(home.join(OsStr::from_bytes(if raw.len() > 2 {
            &raw[2..]
        } else {
            b""
        })))
    } else {
        Ok(path.to_owned())
    }
}
fn home() -> Result<PathBuf> {
    use users::os::unix::UserExt;
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            users::get_user_by_uid(users::get_current_uid())
                .map(|user| user.home_dir().to_path_buf())
        });
    home.filter(|path| path.is_absolute())
        .ok_or_else(|| "absolute home directory unavailable".into())
}
pub(crate) fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
pub(crate) fn nonce() -> Result<String> {
    let mut data = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut data))
        .map_err(err)?;
    Ok(data.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
