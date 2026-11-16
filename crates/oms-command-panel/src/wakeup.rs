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
