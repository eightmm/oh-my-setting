//! Linux metadata hints for a managed Claude Lead's subagent transcript directory.
use super::{claude_project_key, uuid};
use crate::claude_lead_usage::linux::{
    DIR, FILE, config_root, environ, open_dir, start_time, unchanged,
};
use crate::text;
use crate::wakeup::ChildWakeSource;
use oms_panel_core::terminals;
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::inotify::{CreateFlags, ReadFlags, Reader, WatchFlags, add_watch, init};
use rustix::fs::{self, FileType, Mode, Stat};
use rustix::process::geteuid;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const EVENT_LIMIT: usize = 64;
const RECORD_LIMIT: u64 = 16 * 1024;

pub(super) struct Request<'a> {
    pub(super) previous: Option<&'a Arc<SubagentsWatch>>,
}

#[derive(Default)]
struct WatchState {
    incomplete: BTreeSet<String>,
    proven: BTreeSet<String>,
}

pub(crate) struct SubagentsWatch {
    fd: OwnedFd,
    watch: i32,
    uid: u32,
    config: PathBuf,
    root: OwnedFd,
    root_identity: (fs::Dev, u64),
    record_name: String,
    record: Vec<u8>,
    path: PathBuf,
    dir: OwnedFd,
    identity: (fs::Dev, u64),
    state: Mutex<WatchState>,
    lost: AtomicBool,
}

impl SubagentsWatch {
    pub(super) fn observe(
        pid: u32,
        birth: &str,
        repo: &Path,
        request: Request<'_>,
    ) -> Option<Arc<Self>> {
        let uid = geteuid().as_raw();
        let owner = terminals::process(pid).ok()?;
        if owner.uid != uid || owner.pid != pid || owner.birth != birth {
            return None;
        }
        let config = config_root(&environ(pid)?)?;
        let source = Self::prepare_for_uid(&config, pid, birth, repo, request, uid)?;
        (config_root(&environ(pid)?)? == config
            && terminals::process(pid).ok()? == owner
            && source.current())
        .then_some(source)
    }

    #[cfg(test)]
    pub(crate) fn prepare(
        config: &Path,
        pid: u32,
        birth: &str,
        repo: &Path,
        previous: Option<&Arc<Self>>,
    ) -> Option<Arc<Self>> {
        Self::prepare_for_uid(
            config,
            pid,
            birth,
            repo,
            Request { previous },
            geteuid().as_raw(),
        )
    }

    pub(super) fn prepare_for_uid(
        config: &Path,
        pid: u32,
        birth: &str,
        repo: &Path,
        request: Request<'_>,
        uid: u32,
    ) -> Option<Arc<Self>> {
        let root = open_dir(config)?;
        let root_stat = private_dir_for(&root, uid)?;
        let root_identity = (root_stat.st_dev, root_stat.st_ino);
        let record_name = format!("{pid}.json");
        let record = read_session_record(&root, &record_name, uid)?;
        let session = verified_session(&record, pid, birth, repo)?;
        let path = Path::new("projects")
            .join(claude_project_key(repo)?)
            .join(session)
            .join("subagents");
        let (dir, dir_stat) = claude_directory(&root, &path, uid)?;
        let identity = (dir_stat.st_dev, dir_stat.st_ino);
        if let Some(previous) = request.previous.filter(|previous| {
            previous.active()
                && previous.uid == uid
                && previous.config == config
                && previous.root_identity == root_identity
                && previous.record_name == record_name
                && previous.record == record
                && previous.path == path
                && previous.identity == identity
                && previous.current()
        }) {
            return Some(Arc::clone(previous));
        }

        let fd = init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).ok()?;
        let flags = WatchFlags::ONLYDIR
            | WatchFlags::CREATE
            | WatchFlags::MOVED_TO
            | WatchFlags::MODIFY
            | WatchFlags::CLOSE_WRITE
            | WatchFlags::DELETE
            | WatchFlags::MOVED_FROM
            | WatchFlags::ATTRIB
            | WatchFlags::MOVE_SELF
            | WatchFlags::DELETE_SELF;
        let watch = add_watch(&fd, format!("/proc/self/fd/{}", dir.as_raw_fd()), flags).ok()?;
        let source = Arc::new(Self {
            fd,
            watch,
            uid,
            config: config.to_path_buf(),
            root,
            root_identity,
            record_name,
            record,
            path,
            dir,
            identity,
            state: Mutex::default(),
            lost: AtomicBool::new(false),
        });
        source.current().then_some(source)
    }

    pub(crate) fn current(&self) -> bool {
        let Some(named_root) = open_dir(&self.config).and_then(|fd| private_dir_for(&fd, self.uid))
        else {
            return false;
        };
        if (named_root.st_dev, named_root.st_ino) != self.root_identity
            || private_dir_for(&self.root, self.uid)
                .is_none_or(|held| (held.st_dev, held.st_ino) != self.root_identity)
        {
            return false;
        }
        read_session_record(&self.root, &self.record_name, self.uid)
            .is_some_and(|record| record == self.record)
            && private_dir_for(&self.dir, self.uid)
                .is_some_and(|held| (held.st_dev, held.st_ino) == self.identity)
            && claude_directory(&self.root, &self.path, self.uid)
                .is_some_and(|(_, named)| (named.st_dev, named.st_ino) == self.identity)
    }

    pub(crate) fn record_proven<'a>(&self, agents: impl IntoIterator<Item = &'a str>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for id in agents {
            state.incomplete.remove(id);
            if state.proven.len() < EVENT_LIMIT {
                state.proven.insert(id.to_owned());
            }
        }
    }
}

impl ChildWakeSource for SubagentsWatch {
    fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    fn active(&self) -> bool {
        !self.lost.load(Ordering::Relaxed)
    }

    fn changed(&self) -> Option<bool> {
        let mut buffer = [MaybeUninit::uninit(); 1024];
        let mut reader = Reader::new(&self.fd, &mut buffer);
        let mut state = self.state.lock().ok()?;
        let mut changed = false;
        for _ in 0..EVENT_LIMIT {
            let event = match reader.next() {
                Ok(event) => event,
                Err(rustix::io::Errno::AGAIN) => {
                    drop(state);
                    return (!changed || self.current()).then_some(changed);
                }
                Err(_) => return None,
            };
            let flags = event.events();
            if event.wd() != self.watch
                || flags.is_empty()
                || flags.intersects(
                    ReadFlags::IGNORED
                        | ReadFlags::QUEUE_OVERFLOW
                        | ReadFlags::MOVE_SELF
                        | ReadFlags::DELETE_SELF
                        | ReadFlags::UNMOUNT,
                )
            {
                return None;
            }
            let name = event.file_name()?;
            let name = std::ffi::OsStr::from_bytes(name.to_bytes());
            if flags.contains(ReadFlags::ISDIR) {
                continue;
            }
            let path = Path::new(name);
            if let Some(id) = claude_agent_filename_id(path, ".jsonl") {
                if flags.intersects(ReadFlags::DELETE | ReadFlags::MOVED_FROM) {
                    state.incomplete.remove(id);
                    state.proven.remove(id);
                    continue;
                }
                if state.proven.contains(id) {
                    continue;
                }
                if flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO) {
                    if state.incomplete.len() >= EVENT_LIMIT && !state.incomplete.contains(id) {
                        return None;
                    }
                    state.incomplete.insert(id.to_owned());
                    changed = true;
                } else if flags.intersects(ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE)
                    && state.incomplete.contains(id)
                {
                    changed = true;
                }
            } else if let Some(id) = claude_agent_filename_id(path, ".meta.json")
                && flags.intersects(
                    ReadFlags::CREATE
                        | ReadFlags::MOVED_TO
                        | ReadFlags::MODIFY
                        | ReadFlags::CLOSE_WRITE,
                )
                && state.incomplete.contains(id)
            {
                changed = true;
            }
        }
        None
    }

    fn retire(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }
}

fn claude_agent_filename_id<'a>(path: &'a Path, suffix: &str) -> Option<&'a str> {
    let id = path
        .file_name()?
        .to_str()?
        .strip_prefix("agent-")?
        .strip_suffix(suffix)?;
    ((8..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

fn private_dir_for(dir: &OwnedFd, uid: u32) -> Option<Stat> {
    let stat = fs::fstat(dir).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::Directory
        && stat.st_uid == uid
        && stat.st_mode & 0o022 == 0)
        .then_some(stat)
}

fn claude_directory(root: &OwnedFd, path: &Path, uid: u32) -> Option<(OwnedFd, Stat)> {
    let mut names = path.iter();
    let first = fs::openat(root, names.next()?, DIR, Mode::empty()).ok()?;
    let mut stat = private_dir_for(&first, uid)?;
    let dir = names.try_fold(first, |dir, name| {
        let next = fs::openat(&dir, name, DIR, Mode::empty()).ok()?;
        stat = private_dir_for(&next, uid)?;
        Some(next)
    })?;
    Some((dir, stat))
}

fn read_session_record(root: &OwnedFd, name: &str, uid: u32) -> Option<Vec<u8>> {
    let sessions = fs::openat(root, "sessions", DIR, Mode::empty()).ok()?;
    private_dir_for(&sessions, uid)?;
    let fd = fs::openat(&sessions, name, FILE, Mode::empty()).ok()?;
    let before = fs::fstat(&fd).ok()?;
    let size = u64::try_from(before.st_size)
        .ok()
        .filter(|&size| size <= RECORD_LIMIT)?;
    let len = usize::try_from(size).ok()?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_nlink != 1
        || before.st_uid != uid
        || before.st_mode & 0o022 != 0
    {
        return None;
    }
    let file = File::from(fd);
    let mut body = Vec::with_capacity(len + 1);
    (&file).take(size + 1).read_to_end(&mut body).ok()?;
    (body.len() == len && fs::fstat(&file).is_ok_and(|after| unchanged(&before, &after)))
        .then_some(body)
}

fn verified_session(record: &[u8], pid: u32, birth: &str, repo: &Path) -> Option<String> {
    let meta = oms_panel_core::decode(record, RECORD_LIMIT as usize).ok()?;
    let session = text(&meta, "sessionId");
    if meta["pid"].as_u64() != Some(u64::from(pid))
        || !meta["procStart"]
            .as_str()
            .is_some_and(|start| Some(start) == start_time(birth))
        || !uuid(session)
        || Path::new(text(&meta, "cwd")).canonicalize().ok().as_deref() != Some(repo)
    {
        return None;
    }
    Some(session.to_owned())
}
