//! Linux metadata hints for the bound Codex account's already-scanned rollout days.
use super::{codex_directory, codex_filename_id, codex_root_identity, codex_same_identity};
use crate::wakeup::ChildWakeSource;
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::inotify::{CreateFlags, ReadFlags, Reader, WatchFlags, add_watch, init};
use rustix::fs::{self, Stat};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const EVENT_LIMIT: usize = 64;

pub(super) struct Request<'a> {
    pub(super) previous: Option<&'a Arc<DayWatch>>,
}

pub(crate) struct DayWatch {
    fd: OwnedFd,
    home: PathBuf,
    root: OwnedFd,
    days: Vec<Day>,
    incomplete: Mutex<BTreeSet<(i32, OsString)>>,
    lost: AtomicBool,
}

struct Day {
    watch: i32,
    path: PathBuf,
    dir: OwnedFd,
    identity: Stat,
}

impl DayWatch {
    pub(super) fn prepare(
        home: &Path,
        root: &OwnedFd,
        days: &[(PathBuf, OwnedFd)],
        request: Request<'_>,
    ) -> Option<Arc<Self>> {
        if days.is_empty() || days.len() > 3 {
            return None;
        }
        let account = codex_root_identity(home, root)?;
        let identities = days
            .iter()
            .map(|(_, dir)| private_day(dir))
            .collect::<Option<Vec<_>>>()?;
        if let Some(previous) = request.previous.filter(|previous| {
            previous.active()
                && home == previous.home
                && codex_root_identity(home, &previous.root) == Some(account)
                && previous.days.len() == days.len()
                && previous.days.iter().zip(days).zip(&identities).all(
                    |((before, (path, _)), current)| {
                        before.path == *path && codex_same_identity(&before.identity, current)
                    },
                )
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
        let days = days
            .iter()
            .zip(identities)
            .map(|((path, dir), identity)| {
                let watch =
                    add_watch(&fd, format!("/proc/self/fd/{}", dir.as_raw_fd()), flags).ok()?;
                Some(Day {
                    watch,
                    path: path.clone(),
                    dir: dir.try_clone().ok()?,
                    identity,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let source = Arc::new(Self {
            fd,
            home: home.to_path_buf(),
            root: root.try_clone().ok()?,
            days,
            incomplete: Mutex::default(),
            lost: AtomicBool::new(false),
        });
        source.current().then_some(source)
    }

    pub(crate) fn current(&self) -> bool {
        codex_root_identity(&self.home, &self.root).is_some()
            && self.days.iter().all(|day| {
                private_day(&day.dir).is_some_and(|held| codex_same_identity(&day.identity, &held))
                    && codex_directory(&self.root, &day.path)
                        .and_then(|dir| private_day(&dir))
                        .is_some_and(|named| codex_same_identity(&day.identity, &named))
            })
    }

    /// A file created before arming can still have an incomplete first record
    /// during the existing scan. Keep its next write eligible for a wake.
    pub(super) fn record_header(&self, path: &Path, awaiting_record: bool) {
        if let Some(day) = self
            .days
            .iter()
            .find(|day| Some(day.path.as_path()) == path.parent())
            && let Some(name) = path.file_name()
            && let Ok(mut incomplete) = self.incomplete.lock()
        {
            let key = (day.watch, name.to_owned());
            if !awaiting_record {
                incomplete.remove(&key);
            } else if incomplete.len() < EVENT_LIMIT {
                incomplete.insert(key);
            }
        }
    }
}

impl ChildWakeSource for DayWatch {
    fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    fn active(&self) -> bool {
        !self.lost.load(Ordering::Relaxed)
    }

    fn changed(&self) -> Option<bool> {
        let mut buffer = [MaybeUninit::uninit(); 1024];
        let mut reader = Reader::new(&self.fd, &mut buffer);
        let mut incomplete = self.incomplete.lock().ok()?;
        let mut changed = false;
        for _ in 0..EVENT_LIMIT {
            let event = match reader.next() {
                Ok(event) => event,
                Err(rustix::io::Errno::AGAIN) => {
                    drop(incomplete);
                    return (!changed || self.current()).then_some(changed);
                }
                Err(_) => return None,
            };
            let flags = event.events();
            if !self.days.iter().any(|day| event.wd() == day.watch)
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
            let Some(name) = event.file_name() else {
                return None;
            };
            let name = std::ffi::OsStr::from_bytes(name.to_bytes());
            if flags.contains(ReadFlags::ISDIR) || codex_filename_id(Path::new(name)).is_none() {
                continue;
            }
            let key = (event.wd(), name.to_owned());
            if flags.intersects(ReadFlags::DELETE | ReadFlags::MOVED_FROM) {
                incomplete.remove(&key);
                continue;
            }
            if flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO) {
                if incomplete.len() >= EVENT_LIMIT && !incomplete.contains(&key) {
                    return None;
                }
                incomplete.insert(key);
                // A move can publish a fully written file in one operation.
                changed |= flags.contains(ReadFlags::MOVED_TO);
            } else if flags.intersects(ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE)
                && incomplete.contains(&key)
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

fn private_day(dir: &OwnedFd) -> Option<Stat> {
    let stat = fs::fstat(dir).ok()?;
    (fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::Directory
        && stat.st_uid == rustix::process::geteuid().as_raw()
        && stat.st_mode & 0o022 == 0)
        .then_some(stat)
}
