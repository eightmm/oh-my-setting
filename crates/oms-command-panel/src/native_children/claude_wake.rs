//! Linux metadata hints for a managed Claude Lead's subagent transcript directory.
use super::claude_project_key;
use crate::claude_child_chat::{agent_id, verified_session_record};
use crate::claude_lead_usage::linux::{DIR, FILE, config_root, environ, open_dir, unchanged};
use crate::wakeup::ChildWakeSource;
use oms_panel_core::terminals;
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::inotify::{
    CreateFlags, ReadFlags, Reader, WatchFlags, add_watch, init, remove_watch,
};
use rustix::fs::{self, AtFlags, FileType, Mode, Stat};
use rustix::process::geteuid;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const EVENT_LIMIT: usize = 64;
const ENTRY_LIMIT: usize = 2048;
const RECORD_LIMIT: u64 = 16 * 1024;
const SESSION_WATCH_FLAGS: WatchFlags = WatchFlags::ONLYDIR
    .union(WatchFlags::CREATE)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::DELETE_SELF);
const SUBAGENTS_WATCH_FLAGS: WatchFlags = WatchFlags::ONLYDIR
    .union(WatchFlags::CREATE)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::DELETE_SELF);

pub(super) struct Request<'a> {
    pub(super) previous: Option<&'a Arc<SubagentsWatch>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetIdentity {
    Project,
    Session((fs::Dev, u64)),
    Subagents {
        session: (fs::Dev, u64),
        subagents: (fs::Dev, u64),
    },
}

enum Target {
    Project {
        watch: i32,
    },
    Session {
        watch: i32,
        dir: OwnedFd,
        identity: (fs::Dev, u64),
        retired_watch: Option<i32>,
    },
    Subagents {
        watch: i32,
        session_dir: OwnedFd,
        session_identity: (fs::Dev, u64),
        dir: OwnedFd,
        identity: (fs::Dev, u64),
        retired_watch: Option<i32>,
    },
}

struct WatchState {
    target: Target,
    proven: VecDeque<String>,
}

impl WatchState {
    fn remember(&mut self, id: &str) {
        remember_in(&mut self.proven, id);
    }

    fn remember_new(&mut self, id: &str) -> bool {
        if self.proven.iter().any(|entry| entry == id) {
            return false;
        }
        if self.proven.len() >= EVENT_LIMIT {
            self.proven.pop_front();
        }
        self.proven.push_back(id.to_owned());
        true
    }

    fn forget(&mut self, id: &str) {
        if let Some(pos) = self.proven.iter().position(|entry| entry == id) {
            self.proven.remove(pos);
        }
    }
}

fn remember_in(proven: &mut VecDeque<String>, id: &str) {
    if let Some(pos) = proven.iter().position(|entry| entry == id) {
        proven.remove(pos);
    } else if proven.len() >= EVENT_LIMIT {
        proven.pop_front();
    }
    proven.push_back(id.to_owned());
}

pub(crate) struct SubagentsWatch {
    fd: OwnedFd,
    uid: u32,
    config: PathBuf,
    root: OwnedFd,
    root_identity: (fs::Dev, u64),
    record_name: String,
    record: Vec<u8>,
    project_path: PathBuf,
    project_dir: OwnedFd,
    project_identity: (fs::Dev, u64),
    session_name: String,
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
        let session_name = verified_session_record(&record, pid, birth, repo).ok()?;
        let project_path = Path::new("projects").join(claude_project_key(repo)?);
        let (project_dir, project_stat) = claude_directory(&root, &project_path, uid)?;
        let project_identity = (project_stat.st_dev, project_stat.st_ino);
        let session_entry = inspect_child_dir(&project_dir, &session_name, uid)?;
        let expected_target = match &session_entry {
            None => TargetIdentity::Project,
            Some((session_dir, session_stat)) => {
                let session_id = (session_stat.st_dev, session_stat.st_ino);
                match inspect_child_dir(session_dir, "subagents", uid)? {
                    None => TargetIdentity::Session(session_id),
                    Some((_, subagents_stat)) => TargetIdentity::Subagents {
                        session: session_id,
                        subagents: (subagents_stat.st_dev, subagents_stat.st_ino),
                    },
                }
            }
        };
        if let Some(previous) = request.previous.filter(|previous| {
            previous.active()
                && previous.uid == uid
                && previous.config == config
                && previous.root_identity == root_identity
                && previous.record_name == record_name
                && previous.record == record
                && previous.project_path == project_path
                && previous.project_identity == project_identity
                && previous.session_name == session_name
                && previous.matches_target(expected_target)
                && previous.current()
        }) {
            return Some(Arc::clone(previous));
        }

        let fd = init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).ok()?;
        let mut proven = VecDeque::new();
        let target = match session_entry {
            Some((session_dir, session_stat)) => {
                let (target, existing) =
                    arm_session_or_subagents(&fd, session_dir, session_stat, uid, None)?;
                for id in existing {
                    remember_in(&mut proven, &id);
                }
                target
            }
            None => {
                let project_watch = add_watch(
                    &fd,
                    format!("/proc/self/fd/{}", project_dir.as_raw_fd()),
                    SESSION_WATCH_FLAGS,
                )
                .ok()?;
                match inspect_child_dir(&project_dir, &session_name, uid)? {
                    None => Target::Project {
                        watch: project_watch,
                    },
                    Some((session_dir, session_stat)) => {
                        let (target, existing) = arm_session_or_subagents(
                            &fd,
                            session_dir,
                            session_stat,
                            uid,
                            Some(project_watch),
                        )?;
                        let _ = remove_watch(&fd, project_watch);
                        for id in existing {
                            remember_in(&mut proven, &id);
                        }
                        target
                    }
                }
            }
        };
        let source = Arc::new(Self {
            fd,
            uid,
            config: config.to_path_buf(),
            root,
            root_identity,
            record_name,
            record,
            project_path,
            project_dir,
            project_identity,
            session_name,
            state: Mutex::new(WatchState { target, proven }),
            lost: AtomicBool::new(false),
        });
        source.current().then_some(source)
    }

    fn matches_target(&self, expected: TargetIdentity) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        match (&state.target, expected) {
            (Target::Project { .. }, TargetIdentity::Project) => true,
            (Target::Session { identity, .. }, TargetIdentity::Session(expected)) => {
                *identity == expected
            }
            (
                Target::Subagents {
                    session_identity,
                    identity,
                    ..
                },
                TargetIdentity::Subagents { session, subagents },
            ) => *session_identity == session && *identity == subagents,
            _ => false,
        }
    }

    pub(crate) fn current(&self) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        self.current_target(&state.target)
    }

    fn current_target(&self, target: &Target) -> bool {
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
        if read_session_record(&self.root, &self.record_name, self.uid)
            .is_none_or(|record| record != self.record)
            || private_dir_for(&self.project_dir, self.uid)
                .is_none_or(|held| (held.st_dev, held.st_ino) != self.project_identity)
        {
            return false;
        }
        let Some((named_project, named_project_stat)) =
            claude_directory(&self.root, &self.project_path, self.uid)
        else {
            return false;
        };
        if (named_project_stat.st_dev, named_project_stat.st_ino) != self.project_identity {
            return false;
        }
        match target {
            Target::Project { .. } => {
                inspect_child_dir(&named_project, &self.session_name, self.uid).is_some()
            }
            Target::Session { dir, identity, .. } => {
                private_dir_for(dir, self.uid)
                    .is_some_and(|held| (held.st_dev, held.st_ino) == *identity)
                    && open_child_dir(&named_project, &self.session_name, self.uid)
                        .is_some_and(|(_, named)| (named.st_dev, named.st_ino) == *identity)
                    && inspect_child_dir(dir, "subagents", self.uid).is_some()
            }
            Target::Subagents {
                session_dir,
                session_identity,
                dir,
                identity,
                ..
            } => {
                private_dir_for(session_dir, self.uid)
                    .is_some_and(|held| (held.st_dev, held.st_ino) == *session_identity)
                    && open_child_dir(&named_project, &self.session_name, self.uid)
                        .is_some_and(|(_, named)| (named.st_dev, named.st_ino) == *session_identity)
                    && private_dir_for(dir, self.uid)
                        .is_some_and(|held| (held.st_dev, held.st_ino) == *identity)
                    && open_child_dir(session_dir, "subagents", self.uid)
                        .is_some_and(|(_, named)| (named.st_dev, named.st_ino) == *identity)
            }
        }
    }

    pub(crate) fn record_proven<'a>(&self, agents: impl IntoIterator<Item = &'a str>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for id in agents {
            state.remember(id);
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
                    let ok = !changed || self.current_target(&state.target);
                    drop(state);
                    return ok.then_some(changed);
                }
                Err(_) => return None,
            };
            let flags = event.events();
            if flags.is_empty() || flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                return None;
            }
            match &mut state.target {
                Target::Project { watch } => {
                    let watch = *watch;
                    if event.wd() != watch
                        || flags.intersects(
                            ReadFlags::IGNORED
                                | ReadFlags::MOVE_SELF
                                | ReadFlags::DELETE_SELF
                                | ReadFlags::UNMOUNT,
                        )
                    {
                        return None;
                    }
                    let Some(name) = event.file_name() else {
                        if !self.current_target(&state.target) {
                            return None;
                        }
                        continue;
                    };
                    if name.to_bytes() != self.session_name.as_bytes() {
                        continue;
                    }
                    if !flags.contains(ReadFlags::ISDIR)
                        || !flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                    {
                        return None;
                    }
                    let (session_dir, session_stat) =
                        open_child_dir(&self.project_dir, &self.session_name, self.uid)?;
                    let (target, existing) = arm_session_or_subagents(
                        &self.fd,
                        session_dir,
                        session_stat,
                        self.uid,
                        Some(watch),
                    )?;
                    let _ = remove_watch(&self.fd, watch);
                    for id in existing {
                        if state.remember_new(&id) {
                            changed = true;
                        }
                    }
                    state.target = target;
                    if !self.current_target(&state.target) {
                        return None;
                    }
                }
                Target::Session {
                    watch,
                    retired_watch,
                    ..
                } => {
                    if *retired_watch == Some(event.wd()) {
                        if flags.contains(ReadFlags::IGNORED) {
                            *retired_watch = None;
                        }
                        continue;
                    }
                    let watch = *watch;
                    if event.wd() != watch
                        || flags.intersects(
                            ReadFlags::IGNORED
                                | ReadFlags::MOVE_SELF
                                | ReadFlags::DELETE_SELF
                                | ReadFlags::UNMOUNT,
                        )
                    {
                        return None;
                    }
                    let Some(name) = event.file_name() else {
                        if !self.current_target(&state.target) {
                            return None;
                        }
                        continue;
                    };
                    if name.to_bytes() != b"subagents" {
                        continue;
                    }
                    if !flags.contains(ReadFlags::ISDIR)
                        || !flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                    {
                        return None;
                    }
                    let Target::Session {
                        dir: session_dir,
                        identity: session_identity,
                        ..
                    } = std::mem::replace(&mut state.target, Target::Project { watch: -1 })
                    else {
                        unreachable!();
                    };
                    let (dir, stat) = open_child_dir(&session_dir, "subagents", self.uid)?;
                    let (target, existing) = arm_subagents(
                        &self.fd,
                        session_dir,
                        session_identity,
                        dir,
                        stat,
                        Some(watch),
                    )?;
                    let _ = remove_watch(&self.fd, watch);
                    for id in existing {
                        if state.remember_new(&id) {
                            changed = true;
                        }
                    }
                    state.target = target;
                    if !self.current_target(&state.target) {
                        return None;
                    }
                }
                Target::Subagents {
                    watch,
                    retired_watch,
                    ..
                } => {
                    if *retired_watch == Some(event.wd()) {
                        if flags.contains(ReadFlags::IGNORED) {
                            *retired_watch = None;
                        }
                        continue;
                    }
                    if event.wd() != *watch
                        || flags.intersects(
                            ReadFlags::IGNORED
                                | ReadFlags::MOVE_SELF
                                | ReadFlags::DELETE_SELF
                                | ReadFlags::UNMOUNT,
                        )
                    {
                        return None;
                    }
                    let Some(name) = event.file_name() else {
                        if !self.current_target(&state.target) {
                            return None;
                        }
                        continue;
                    };
                    if flags.contains(ReadFlags::ISDIR) {
                        continue;
                    }
                    let name = std::ffi::OsStr::from_bytes(name.to_bytes());
                    if let Some(id) = claude_agent_filename_id(Path::new(name)) {
                        if flags.intersects(ReadFlags::DELETE | ReadFlags::MOVED_FROM) {
                            state.forget(id);
                        } else if flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                            && state.remember_new(id)
                        {
                            changed = true;
                        }
                    }
                }
            }
        }
        None
    }

    fn retire(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }
}

fn claude_agent_filename_id(path: &Path) -> Option<&str> {
    let id = path
        .file_name()?
        .to_str()?
        .strip_prefix("agent-")?
        .strip_suffix(".jsonl")?;
    agent_id(id).then_some(id)
}

fn arm_session_or_subagents(
    fd: &OwnedFd,
    session_dir: OwnedFd,
    session_stat: Stat,
    uid: u32,
    retired_watch: Option<i32>,
) -> Option<(Target, Vec<String>)> {
    let session_identity = (session_stat.st_dev, session_stat.st_ino);
    if let Some((dir, stat)) = inspect_child_dir(&session_dir, "subagents", uid)? {
        return arm_subagents(fd, session_dir, session_identity, dir, stat, retired_watch);
    }
    let session_watch = add_watch(
        fd,
        format!("/proc/self/fd/{}", session_dir.as_raw_fd()),
        SESSION_WATCH_FLAGS,
    )
    .ok()?;
    match inspect_child_dir(&session_dir, "subagents", uid) {
        Some(None) => Some((
            Target::Session {
                watch: session_watch,
                dir: session_dir,
                identity: session_identity,
                retired_watch,
            },
            Vec::new(),
        )),
        Some(Some((dir, stat))) => {
            let result = arm_subagents(
                fd,
                session_dir,
                session_identity,
                dir,
                stat,
                Some(session_watch),
            );
            let _ = remove_watch(fd, session_watch);
            result
        }
        None => {
            let _ = remove_watch(fd, session_watch);
            None
        }
    }
}

fn arm_subagents(
    fd: &OwnedFd,
    session_dir: OwnedFd,
    session_identity: (fs::Dev, u64),
    dir: OwnedFd,
    stat: Stat,
    retired_watch: Option<i32>,
) -> Option<(Target, Vec<String>)> {
    let watch = add_watch(
        fd,
        format!("/proc/self/fd/{}", dir.as_raw_fd()),
        SUBAGENTS_WATCH_FLAGS,
    )
    .ok()?;
    let Some(existing) = scan_agent_ids(&dir) else {
        let _ = remove_watch(fd, watch);
        return None;
    };
    Some((
        Target::Subagents {
            watch,
            session_dir,
            session_identity,
            dir,
            identity: (stat.st_dev, stat.st_ino),
            retired_watch,
        },
        existing,
    ))
}

fn private_dir_for(dir: &OwnedFd, uid: u32) -> Option<Stat> {
    let stat = fs::fstat(dir).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::Directory
        && stat.st_uid == uid
        && stat.st_mode & 0o022 == 0)
        .then_some(stat)
}

fn open_child_dir(parent: &OwnedFd, name: &str, uid: u32) -> Option<(OwnedFd, Stat)> {
    let dir = fs::openat(parent, name, DIR, Mode::empty()).ok()?;
    let stat = private_dir_for(&dir, uid)?;
    Some((dir, stat))
}

fn inspect_child_dir(parent: &OwnedFd, name: &str, uid: u32) -> Option<Option<(OwnedFd, Stat)>> {
    match fs::openat(parent, name, DIR, Mode::empty()) {
        Ok(dir) => private_dir_for(&dir, uid).map(|stat| Some((dir, stat))),
        Err(rustix::io::Errno::NOENT) => matches!(
            fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW),
            Err(rustix::io::Errno::NOENT)
        )
        .then_some(None),
        Err(_) => None,
    }
}

fn scan_agent_ids(dir: &OwnedFd) -> Option<Vec<String>> {
    let opened = fs::openat(dir, ".", DIR, Mode::empty()).ok()?;
    let entries = fs::Dir::read_from(opened).ok()?;
    let mut ids = Vec::new();
    for entry in entries.take(ENTRY_LIMIT).flatten() {
        let raw = entry.file_name();
        let name = std::ffi::OsStr::from_bytes(raw.to_bytes());
        if let Some(id) = claude_agent_filename_id(Path::new(name)) {
            ids.push((raw.to_owned(), id.to_owned()));
        }
    }
    if ids.len() > EVENT_LIMIT {
        let mut stamped: Vec<_> = ids
            .into_iter()
            .map(|(raw, id)| {
                let order = fs::statat(dir, &raw, AtFlags::SYMLINK_NOFOLLOW)
                    .ok()
                    .map(|st| (st.st_mtime, st.st_mtime_nsec));
                (order, id)
            })
            .collect();
        stamped.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        return Some(stamped.into_iter().map(|(_, id)| id).collect());
    }
    Some(ids.into_iter().map(|(_, id)| id).collect())
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
