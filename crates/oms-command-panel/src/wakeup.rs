//! Non-blocking wakeup socket for `oms panel --watch`.
//!
//! A watcher binds a datagram socket at `<repo>/.oms/panel-state/wakeup.sock`
//! (mode 0600, inside a directory it owns) and polls it together with its
//! other descriptors, so a producer's one-byte datagram refreshes the board in
//! well under the polling cadence. Everything is best effort: when the socket
//! cannot exist (path longer than `sun_path`, foreign file in the way, another
//! live watcher) the watcher keeps its bounded polling cadence.
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use std::fs::{self, DirBuilder, Metadata};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SOCKET: &str = "wakeup.sock";
/// Burst window after the first datagram: a producer that writes several
/// events back to back costs one refresh, delayed by at most this long.
pub(crate) const COALESCE: Duration = Duration::from_millis(15);
/// Longest single blocking wait, so interruption is observed every slice.
pub(crate) const SLICE: Duration = Duration::from_millis(100);
/// `sun_path` capacity including the terminating NUL: 108 on Linux, 104 on
/// macOS and the BSDs. Longer paths cannot be bound or connected.
#[cfg(target_os = "linux")]
const SUN_PATH: usize = 108;
#[cfg(not(target_os = "linux"))]
const SUN_PATH: usize = 104;

/// Absolute spelling is the caller's; the socket exists only if it fits `sun_path`.
pub fn socket_path(repo: &Path) -> PathBuf {
    repo.join(".oms/panel-state").join(SOCKET)
}
fn fits(path: &Path) -> bool {
    path.as_os_str().as_bytes().len() < SUN_PATH
}
fn mine(meta: &Metadata) -> bool {
    meta.uid() == rustix::process::getuid().as_raw()
}
fn private_dir(path: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    // lstat: a symlinked component is never followed.
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && mine(&meta) && meta.mode() & 0o022 == 0 {
        Ok(())
    } else {
        Err(io::Error::other(
            "wakeup directory is not an owner-only directory",
        ))
    }
}
fn own_socket(path: &Path) -> Option<Metadata> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.file_type().is_socket() && mine(m))
}

/// A bound wakeup socket; the file is removed on drop if it is still ours.
pub struct Wakeup {
    socket: UnixDatagram,
    path: PathBuf,
    identity: (u64, u64),
}
impl Wakeup {
    /// Bind the watcher socket, or `None` when it cannot safely exist.
    pub fn bind(repo: &Path) -> Option<Self> {
        let path = socket_path(repo);
        if !fits(&path) {
            return None;
        }
        private_dir(&repo.join(".oms")).ok()?;
        private_dir(path.parent()?).ok()?;
        if let Ok(existing) = fs::symlink_metadata(&path) {
            // Only our own socket whose owner is gone is replaced; a live
            // watcher keeps its wakeups, a foreign file is never touched.
            let stale = existing.file_type().is_socket()
                && mine(&existing)
                && UnixDatagram::unbound()
                    .and_then(|probe| probe.connect(&path))
                    .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused);
            if !stale {
                return None;
            }
            fs::remove_file(&path).ok()?;
        }
        let socket = UnixDatagram::bind(&path).ok()?;
        let made = own_socket(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).ok()?;
        socket.set_nonblocking(true).ok()?;
        Some(Self {
            socket,
            path,
            identity: (made.dev(), made.ino()),
        })
    }
    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
    /// Consume every queued datagram; true if there was at least one.
    pub(crate) fn drain(&self) -> bool {
        let mut woke = false;
        let mut byte = [0u8; 64];
        while self.socket.recv(&mut byte).is_ok() {
            woke = true;
        }
        woke
    }
    /// Drain, then absorb the rest of a burst for up to [`COALESCE`].
    pub(crate) fn drain_burst(&self) -> bool {
        if !self.drain() {
            return false;
        }
        let end = Instant::now() + COALESCE;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut fds = [PollFd::new(&self.socket, PollFlags::IN)];
            match poll(&mut fds, Some(&timespec(left))) {
                Ok(n) if n > 0 => {
                    self.drain();
                }
                _ => break,
            }
        }
        true
    }
}
impl Drop for Wakeup {
    fn drop(&mut self) {
        if let Some(m) = own_socket(&self.path)
            && (m.dev(), m.ino()) == self.identity
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: duration.as_secs() as _,
        tv_nsec: duration.subsec_nanos() as _,
    }
}

/// Wait up to `duration` (at most one [`SLICE`]) for a wakeup. Without a
/// socket this is a bounded sleep. True when woken; interruption ends early.
pub fn wait(wake: Option<&Wakeup>, duration: Duration, interrupted: &AtomicBool) -> bool {
    let duration = duration.min(SLICE);
    if interrupted.load(Ordering::Acquire) {
        return false;
    }
    let Some(wake) = wake else {
        std::thread::sleep(duration);
        return false;
    };
    let mut fds = [PollFd::new(&wake.socket, PollFlags::IN)];
    match poll(&mut fds, Some(&timespec(duration))) {
        Ok(n) if n > 0 => wake.drain_burst(),
        _ => false,
    }
}

/// Best-effort one-byte wake for a panel watcher of `repo`. It never blocks,
/// never creates anything and never fails the caller when nobody listens.
pub fn notify_wakeup(repo: &Path) {
    let path = socket_path(repo);
    if !fits(&path) || own_socket(&path).is_none() {
        return;
    }
    if let Ok(socket) = UnixDatagram::unbound()
        && socket.set_nonblocking(true).is_ok()
    {
        let _ = socket.send_to(&[1], &path);
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct AgyRosterWatch {
    fd: rustix::fd::OwnedFd,
    watch: i32,
    lost: AtomicBool,
}

#[cfg(target_os = "linux")]
impl AgyRosterWatch {
    /// Arm on the already-opened parent transcript, never by reopening its name.
    /// Callers must still confirm that the descriptor is the current transcript.
    pub(crate) fn arm(file: &fs::File) -> Option<Arc<Self>> {
        use rustix::fs::inotify::{CreateFlags, WatchFlags, add_watch, init};
        use std::os::fd::AsRawFd;

        let fd = init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).ok()?;
        let watch = add_watch(
            &fd,
            format!("/proc/self/fd/{}", file.as_raw_fd()),
            WatchFlags::MODIFY
                | WatchFlags::CLOSE_WRITE
                | WatchFlags::ATTRIB
                | WatchFlags::MOVE_SELF
                | WatchFlags::DELETE_SELF,
        )
        .ok()?;
        Some(Arc::new(Self {
            fd,
            watch,
            lost: AtomicBool::new(false),
        }))
    }

    pub(crate) fn active(&self) -> bool {
        !self.lost.load(Ordering::Relaxed)
    }

    fn changed(&self) -> Option<bool> {
        use rustix::fs::inotify::{ReadFlags, Reader};
        use std::mem::MaybeUninit;

        let mut buf = [MaybeUninit::uninit(); 1024];
        let mut reader = Reader::new(&self.fd, &mut buf);
        let writes = ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE;
        let mut changed = false;
        for _ in 0..64 {
            let event = match reader.next() {
                Ok(event) => event,
                Err(rustix::io::Errno::AGAIN) => return Some(changed),
                Err(_) => return None,
            };
            if event.wd() != self.watch
                || event.file_name().is_some()
                || event.events().is_empty()
                || !writes.contains(event.events())
            {
                // Identity changes and queue loss use the normal refresh fallback.
                return None;
            }
            changed = true;
        }
        None
    }
}

#[cfg(target_os = "linux")]
pub(crate) trait ChildWakeSource {
    fn fd(&self) -> BorrowedFd<'_>;
    fn active(&self) -> bool;
    fn changed(&self) -> Option<bool>;
    fn retire(&self);
}

#[cfg(target_os = "linux")]
impl ChildWakeSource for AgyRosterWatch {
    fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    fn active(&self) -> bool {
        self.active()
    }

    fn changed(&self) -> Option<bool> {
        self.changed()
    }

    fn retire(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct FirstChildWake<S> {
    source: Option<Arc<S>>,
    dirty: bool,
    next_refresh: Option<Instant>,
}

#[cfg(target_os = "linux")]
impl<S> Default for FirstChildWake<S> {
    fn default() -> Self {
        Self {
            source: None,
            dirty: false,
            next_refresh: None,
        }
    }
}

#[cfg(target_os = "linux")]
impl<S: ChildWakeSource> FirstChildWake<S> {
    pub(crate) fn replace(&mut self, source: Option<Arc<S>>) {
        let source = source.filter(|source| source.active());
        if !self
            .source
            .as_ref()
            .zip(source.as_ref())
            .is_some_and(|(before, after)| Arc::ptr_eq(before, after))
        {
            self.dirty = false;
        }
        self.source = source;
    }

    pub(crate) fn fd(&self) -> Option<BorrowedFd<'_>> {
        // Once dirty, wait for the permitted refresh without waking on each write.
        self.source
            .as_ref()
            .filter(|_| !self.dirty)
            .map(|source| source.fd())
    }

    pub(crate) fn refresh(&mut self, readable: bool, idle: bool, now: Instant) -> bool {
        let dispatch = idle && self.next_refresh.is_none_or(|after| now >= after);
        if (readable || (dispatch && self.dirty))
            && let Some(source) = &self.source
        {
            if let Some(changed) = source.changed() {
                self.dirty |= changed;
            } else {
                source.retire();
                self.replace(None);
            }
        }
        if dispatch && std::mem::take(&mut self.dirty) {
            self.next_refresh = Some(now + Duration::from_secs(1));
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Self {
            // Short name: a macOS temp root already uses most of `sun_path`.
            let p = std::env::temp_dir().join(format!("ow{}{label}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn first_child_wake_coalesces_writes_and_discards_a_lost_watch() {
        let dir = Dir::new("j");
        let file = dir.0.join("parent");
        fs::write(&file, b"empty roster").unwrap();
        let source = AgyRosterWatch::arm(&fs::File::open(&file).unwrap()).unwrap();
        let mut wake = FirstChildWake::<AgyRosterWatch>::default();
        wake.replace(Some(source.clone()));
        let ready = |wake: &FirstChildWake<AgyRosterWatch>| {
            wake.fd().is_some_and(|fd| {
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                poll(&mut fds, Some(&timespec(Duration::ZERO))).unwrap() > 0
            })
        };
        let now = Instant::now();
        fs::write(dir.0.join("foreign"), b"other transcript").unwrap();
        assert!(!ready(&wake));

        fs::write(&file, b"write while busy").unwrap();
        assert!(ready(&wake));
        assert!(!wake.refresh(true, false, now));
        assert!(wake.fd().is_none());
        fs::write(&file, b"second write").unwrap();
        assert!(wake.refresh(false, true, now));
        assert!(!wake.refresh(ready(&wake), true, now));

        fs::write(&file, b"write during the rate limit").unwrap();
        assert!(ready(&wake));
        assert!(!wake.refresh(true, true, now));
        fs::rename(&file, dir.0.join("moved")).unwrap();
        fs::write(&file, b"unverified replacement").unwrap();
        assert!(!wake.refresh(false, true, now + Duration::from_secs(1)));
        assert!(wake.fd().is_none());
        assert!(!source.active());
        wake.replace(Some(source));
        assert!(wake.fd().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn first_child_wake_wakes_on_claude_child_transcript_and_ignores_symlinked_dir() {
        use crate::native_children::ClaudeSubagentsWatch;
        use serde_json::json;

        let dir = Dir::new("k");
        let base = dir.0.canonicalize().unwrap();
        let repo = base.join("repo");
        let config = base.join("claude");
        let sessions = config.join("sessions");
        let key: String = repo
            .to_str()
            .unwrap()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let session = "0a1b2c3d-4e5f-6789-abcd-ef0123456789";
        let session_dir = config.join("projects").join(key).join(session);
        let subagents = session_dir.join("subagents");
        let elsewhere = base.join("elsewhere");
        for path in [&repo, &config, &sessions, &session_dir, &elsewhere] {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .unwrap();
        }
        let pid = 4242u32;
        let birth = "fixture-boot:987654";
        let record = sessions.join(format!("{pid}.json"));
        fs::write(
            &record,
            json!({
                "pid": pid,
                "procStart": "987654",
                "sessionId": session,
                "cwd": repo.to_str().unwrap(),
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();

        std::os::unix::fs::symlink(&elsewhere, &subagents).unwrap();
        assert!(ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).is_none());
        let mut wake = FirstChildWake::<ClaudeSubagentsWatch>::default();
        assert!(wake.fd().is_none());
        fs::write(
            subagents.join("agent-a1b2c3d4e5f60718.jsonl"),
            b"not read\n",
        )
        .unwrap();
        assert!(!wake.refresh(true, true, Instant::now()));

        fs::remove_file(&subagents).unwrap();
        DirBuilder::new().mode(0o700).create(&subagents).unwrap();
        let source = ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).unwrap();
        wake.replace(Some(source));
        let ready = |wake: &FirstChildWake<ClaudeSubagentsWatch>| {
            wake.fd().is_some_and(|fd| {
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                poll(&mut fds, Some(&timespec(Duration::ZERO))).unwrap() > 0
            })
        };
        let now = Instant::now();
        fs::write(subagents.join("foreign.txt"), b"ignored").unwrap();
        assert!(!wake.refresh(ready(&wake), true, now));

        let child = subagents.join("agent-a1b2c3d4e5f60718.jsonl");
        fs::write(&child, [0xff, 0xfe]).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(ready(&wake));
        assert!(wake.refresh(true, true, now));
        fs::set_permissions(&child, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn claude_wake_handles_missing_session_directory_and_unsafe_targets() {
        use crate::native_children::ClaudeSubagentsWatch;
        use serde_json::json;

        let dir = Dir::new("claude-lazy-session");
        let repo = dir.0.join("repo");
        let config = dir.0.join("claude");
        let sessions = config.join("sessions");
        let projects = config.join("projects");
        let elsewhere = dir.0.join("elsewhere");
        for path in [&repo, &config, &sessions, &projects, &elsewhere] {
            DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        let repo = fs::canonicalize(&repo).unwrap();
        let key = crate::native_children::claude_project_key(&repo).unwrap();
        let project = projects.join(&key);
        DirBuilder::new().mode(0o700).create(&project).unwrap();

        let pid = 4343u32;
        let birth = "fixture-boot:112233";
        let session = "22345678-1234-4234-8234-123456789abc";
        let record = sessions.join(format!("{pid}.json"));
        fs::write(
            &record,
            json!({
                "pid": pid,
                "procStart": "112233",
                "sessionId": session,
                "cwd": repo.to_str().unwrap(),
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();

        // Missing <session>/ arms a bounded watch on projects/<key>/ and ignores foreign entries.
        let source = ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).unwrap();
        let mut wake = FirstChildWake::<ClaudeSubagentsWatch>::default();
        wake.replace(Some(Arc::clone(&source)));
        let ready = |wake: &FirstChildWake<ClaudeSubagentsWatch>| {
            wake.fd().is_some_and(|fd| {
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                poll(&mut fds, Some(&timespec(Duration::ZERO))).unwrap() > 0
            })
        };
        let now = Instant::now();
        let foreign = project.join("other-session");
        DirBuilder::new().mode(0o700).create(&foreign).unwrap();
        assert!(!wake.refresh(ready(&wake), true, now));

        // Creating <session>/ -> subagents/ -> agent-*.jsonl transitions and wakes on the first child.
        let session_dir = project.join(session);
        DirBuilder::new().mode(0o700).create(&session_dir).unwrap();
        assert!(!wake.refresh(ready(&wake), true, now));
        assert!(ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, Some(&source)).is_some());

        let subagents = session_dir.join("subagents");
        DirBuilder::new().mode(0o700).create(&subagents).unwrap();
        assert!(!wake.refresh(ready(&wake), true, now));

        let child = subagents.join("agent-b1c2d3e4f5061728.jsonl");
        fs::write(&child, b"{}\n").unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ready(&wake));
        assert!(wake.refresh(true, true, now));

        // Reset to missing <session>/, then create <session>/subagents/agent-*.jsonl in one burst.
        fs::remove_dir_all(&session_dir).unwrap();
        let burst = ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).unwrap();
        wake.replace(Some(burst));
        DirBuilder::new().mode(0o700).create(&session_dir).unwrap();
        DirBuilder::new().mode(0o700).create(&subagents).unwrap();
        fs::write(&child, b"{}\n").unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ready(&wake));
        assert!(wake.refresh(true, true, now + Duration::from_secs(1)));

        // Symlinked or group-writable <session>/ fails closed.
        fs::remove_dir_all(&session_dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &session_dir).unwrap();
        assert!(ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).is_none());
        fs::remove_file(&session_dir).unwrap();
        DirBuilder::new().mode(0o750).create(&session_dir).unwrap();
        fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(ClaudeSubagentsWatch::prepare(&config, pid, birth, &repo, None).is_none());
    }

    #[test]
    fn notify_wakes_a_waiting_watcher_quickly() {
        let dir = Dir::new("a");
        let wake = Wakeup::bind(&dir.0).expect("short path binds");
        let mode = fs::symlink_metadata(socket_path(&dir.0)).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600);
        let stop = AtomicBool::new(false);
        let sent = std::thread::scope(|scope| {
            let notifier = scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(30));
                let at = Instant::now();
                notify_wakeup(&dir.0);
                at
            });
            let mut woke = false;
            for _ in 0..50 {
                if wait(Some(&wake), SLICE, &stop) {
                    woke = true;
                    break;
                }
            }
            assert!(woke, "watcher was never woken");
            (notifier.join().unwrap(), Instant::now())
        });
        assert!(
            sent.1.duration_since(sent.0) < Duration::from_millis(100),
            "wake took {:?}",
            sent.1.duration_since(sent.0)
        );
    }

    #[test]
    fn a_burst_costs_one_wake_and_leaves_no_residue() {
        let dir = Dir::new("b");
        let wake = Wakeup::bind(&dir.0).unwrap();
        for _ in 0..20 {
            notify_wakeup(&dir.0);
        }
        let stop = AtomicBool::new(false);
        assert!(wait(Some(&wake), SLICE, &stop));
        assert!(!wake.drain(), "burst was fully coalesced");
        assert!(!wait(Some(&wake), Duration::from_millis(5), &stop));
    }

    #[test]
    fn notify_without_a_watcher_is_silent() {
        let dir = Dir::new("c");
        notify_wakeup(&dir.0);
        notify_wakeup(&dir.0.join("missing"));
        assert!(!dir.0.join(".oms").exists(), "notify creates nothing");
        let wake = Wakeup::bind(&dir.0).unwrap();
        drop(wake);
        assert!(!socket_path(&dir.0).exists(), "drop removes the socket");
        notify_wakeup(&dir.0);
    }

    #[test]
    fn long_paths_fall_back_to_bounded_sleep() {
        let dir = Dir::new("d");
        let long = dir.0.join("x".repeat(SUN_PATH));
        fs::create_dir(&long).unwrap();
        assert!(!fits(&socket_path(&long)));
        assert!(Wakeup::bind(&long).is_none());
        notify_wakeup(&long);
        let stop = AtomicBool::new(false);
        let begun = Instant::now();
        assert!(!wait(None, Duration::from_millis(20), &stop));
        assert!(begun.elapsed() >= Duration::from_millis(20));
        stop.store(true, Ordering::Release);
        let begun = Instant::now();
        assert!(!wait(None, SLICE, &stop));
        assert!(begun.elapsed() < SLICE, "interruption skips the wait");
    }

    #[test]
    fn stale_sockets_are_replaced_but_live_or_foreign_ones_are_not() {
        let dir = Dir::new("e");
        let first = Wakeup::bind(&dir.0).unwrap();
        assert!(
            Wakeup::bind(&dir.0).is_none(),
            "a live watcher keeps its socket"
        );
        // Leave a dead socket file behind, as a killed watcher would.
        let path = socket_path(&dir.0);
        drop(first);
        let dead = UnixDatagram::bind(&path).unwrap();
        drop(dead);
        assert!(path.exists());
        assert!(
            Wakeup::bind(&dir.0).is_some(),
            "dead own socket is replaced"
        );
        fs::remove_file(&path).ok();
        fs::write(&path, b"not a socket").unwrap();
        assert!(Wakeup::bind(&dir.0).is_none());
        assert_eq!(fs::read(&path).unwrap(), b"not a socket");
    }

    #[test]
    fn symlinked_state_directory_is_refused() {
        let dir = Dir::new("f");
        let elsewhere = dir.0.join("t");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir(dir.0.join(".oms")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.0.join(".oms/panel-state")).unwrap();
        assert!(Wakeup::bind(&dir.0).is_none());
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }
}
