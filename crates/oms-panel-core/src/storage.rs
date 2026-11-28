use crate::{Error, Result, Value, decode, identifier, require};
use oms_lock::{Guard, Lock, Mode, Settings};
use oms_safe_fs::{ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAX_STATE: usize = 8 * 1024 * 1024;
/// Read caller-supplied scratch input through one descriptor, including external
/// files. NONBLOCK avoids hanging on a FIFO before the regular-file check.
pub(crate) fn input(repo: &Path, path: &Path, maximum: usize) -> Result<Vec<u8>> {
    use rustix::fs::{self, FileType, Mode, OFlags};
    use std::io::Read;
    let target = if path.is_absolute() {
        path.to_owned()
    } else {
        repo.join(path)
    };
    let fd = fs::open(
        &target,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(Error::new)?;
    let before = fs::fstat(&fd).map_err(Error::new)?;
    require(
        FileType::from_raw_mode(before.st_mode) == FileType::RegularFile
            && before.st_nlink == 1
            && before.st_uid == rustix::process::getuid().as_raw()
            && before.st_size >= 0
            && before.st_size as u64 <= maximum as u64,
        "input must be an owned single-link bounded regular file",
    )?;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::new)?;
    let after = fs::fstat(&file).map_err(Error::new)?;
    require(
        before.st_dev == after.st_dev
            && before.st_ino == after.st_ino
            && before.st_size == after.st_size
            && before.st_mtime == after.st_mtime
            && before.st_mtime_nsec == after.st_mtime_nsec
            && before.st_ctime == after.st_ctime
            && before.st_ctime_nsec == after.st_ctime_nsec
            && after.st_nlink == 1
            && bytes.len() as u64 == before.st_size as u64
            && bytes.len() <= maximum,
        "input changed while reading",
    )?;
    require(!bytes.contains(&0), "input contains NUL")?;
    crate::outbound(&bytes)?;
    Ok(bytes)
}
/// A captured physical root. No creation or repair occurs at open.
pub struct Store {
    root: Root,
    settings: Settings,
}
pub struct Transaction<'a> {
    store: &'a Store,
    guard: Guard,
    hold_deadline: Option<Instant>,
    interrupted: Option<&'a AtomicBool>,
}
pub(crate) fn reservation_error(error: oms_command_telemetry::store::dispatch::Error) -> Error {
    Error {
        code: error.exit_code(),
        publication_uncertain: error.publication_uncertain,
        ..Error::new(error)
    }
}
impl Store {
    pub fn open(repo: &Path) -> Result<Self> {
        Self::with_settings(repo, Settings::from_env().map_err(Error::new)?)
    }
    pub fn with_settings(repo: &Path, settings: Settings) -> Result<Self> {
        Ok(Self {
            root: Root::open(repo).map_err(|e| Error::new(format!("panel root: {e:?}")))?,
            settings,
        })
    }
    pub fn repo(&self) -> &Path {
        self.root.physical_root()
    }
    pub(crate) fn dispatch_transaction<'a>(
        &self,
        interrupted: &'a AtomicBool,
    ) -> Result<oms_command_telemetry::store::dispatch::Transaction<'a>> {
        oms_command_telemetry::store::dispatch::Transaction::with_settings(
            self.repo(),
            self.settings.clone(),
            Duration::from_secs(10),
            interrupted,
        )
        .map_err(reservation_error)
    }
    pub fn read(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        path(relative)?;
        self.root
            .read_no_follow(
                relative,
                ByteReadOptions {
                    max_bytes: maximum,
                    missing_ok: missing,
                },
            )
            .map_err(|e| Error::new(format!("panel read refused: {e:?}")))
    }
    pub fn read_jsonl(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        path(relative)?;
        self.root
            .read_no_follow_jsonl(
                relative,
                ByteReadOptions {
                    max_bytes: maximum,
                    missing_ok: missing,
                },
            )
            .map_err(|e| Error::new(format!("panel read refused: {e:?}")))
    }
    pub fn json(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Option<Value>> {
        let bytes = self.read(relative, maximum, missing)?;
        if missing && bytes.is_empty() {
            Ok(None)
        } else {
            decode(&bytes, maximum).map(Some)
        }
    }
    /// Reads a single-link file owned by the current user; a missing file is `None`.
    pub fn json_owned(&self, relative: &Path, maximum: usize) -> Result<Option<Value>> {
        path(relative)?;
        let policy = oms_safe_fs::Policy {
            single_link: true,
            owner: Some(rustix::process::getuid().as_raw()),
            max_components: None,
        };
        match self.root.read_path(relative, maximum, policy) {
            Ok(bytes) => decode(&bytes, maximum).map(Some),
            Err(oms_safe_fs::Error::Missing) => Ok(None),
            Err(e) => Err(Error::new(format!("panel read refused: {e:?}"))),
        }
    }
    /// Direct flock shares the Python panel lock, independent of the shell lock selector.
    pub fn transaction(&self, name: &str, timeout: Duration) -> Result<Transaction<'_>> {
        identifier(name)?;
        require(!timeout.is_zero(), "panel lock timeout must be positive")?;
        self.lock_at(Path::new(&format!(".oms/artifacts/{name}.lock")), timeout)
    }
    pub fn lock_at(&self, relative: &Path, timeout: Duration) -> Result<Transaction<'_>> {
        path(relative)?;
        require(!timeout.is_zero(), "panel lock timeout must be positive")?;
        self.lock_at_raw(relative, timeout).map_err(|e| Error {
            code: e.exit_code(),
            ..Error::new(e)
        })
    }
    pub fn lock_at_bounded<'a>(
        &'a self,
        relative: &Path,
        interrupted: &'a AtomicBool,
        deadline: Instant,
        bounds: oms_command_plan::team::AdmissionBounds,
    ) -> Result<Transaction<'a>> {
        path(relative)?;
        let mut settings = self.settings.clone();
        settings.mode = Mode::Flock;
        let wait_deadline = bounds.wait_deadline(deadline);
        let guard = oms_command_plan::team::acquire_bounded_lock(
            settings,
            interrupted,
            deadline,
            wait_deadline,
            "panel lock is busy",
            |settings| Lock::in_root(self.repo().to_path_buf(), relative, settings),
        )
        .map_err(|e| Error {
            code: e.code,
            ..Error::new(e.message)
        })?;
        let tx = Transaction {
            store: self,
            guard,
            hold_deadline: Some(bounds.hold_deadline(deadline)),
            interrupted: Some(interrupted),
        };
        tx.check()?;
        Ok(tx)
    }
    pub(crate) fn lock_at_raw(
        &self,
        relative: &Path,
        timeout: Duration,
    ) -> std::result::Result<Transaction<'_>, oms_lock::Error> {
        path(relative).map_err(|_| oms_lock::Error::InvalidTarget)?;
        if timeout.is_zero() {
            return Err(oms_lock::Error::InvalidSettings);
        }
        let mut settings = self.settings.clone();
        settings.mode = Mode::Flock;
        settings.timeout = timeout;
        let guard = Lock::in_root(self.repo().to_path_buf(), relative, settings)?.acquire(None)?;
        Ok(Transaction {
            store: self,
            guard,
            hold_deadline: None,
            interrupted: None,
        })
    }
}
pub(crate) fn path(relative: &Path) -> Result<()> {
    require(
        !relative.as_os_str().is_empty()
            && !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "panel path must be repository-relative",
    )
}
impl Transaction<'_> {
    pub fn check(&self) -> Result<()> {
        if self
            .interrupted
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err(Error {
                code: 130,
                ..Error::new("team admission interrupted")
            });
        }
        if self
            .hold_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Error {
                code: 75,
                ..Error::new("team admission deadline expired")
            });
        }
        self.check_raw().map_err(|e| Error {
            code: e.exit_code(),
            ..Error::new(e)
        })
    }
    pub(crate) fn check_raw(&self) -> std::result::Result<(), oms_lock::Error> {
        self.guard.check()
    }
    pub fn read(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Vec<u8>> {
        self.check()?;
        self.store.read(relative, maximum, missing)
    }
    pub fn json(&self, relative: &Path, maximum: usize, missing: bool) -> Result<Option<Value>> {
        self.check()?;
        self.store.json(relative, maximum, missing)
    }
    pub fn mutate(
        &self,
        relative: &Path,
        maximum: usize,
        transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        path(relative)?;
        self.check()?;
        self.store
            .root
            .atomic_mutate(
                relative,
                MutationOptions {
                    read: ByteReadOptions {
                        missing_ok: true,
                        max_bytes: maximum,
                    },
                    create_parent: true,
                    max_output: maximum,
                    ..MutationOptions::default()
                },
                |old| {
                    let next = transform(old)?;
                    self.check()?;
                    Ok(next)
                },
            )
            .map(|v| v.bytes)
            .map_err(|e| match e {
                MutationError::Transform(e) => e,
                MutationError::Storage { phase, error } => Error {
                    publication_uncertain: phase != MutationPhase::BeforePublication,
                    ..Error::new(format!(
                        "panel mutation {phase:?}: {error:?}; reconcile before retry"
                    ))
                },
            })
    }
    pub fn write_json(
        &self,
        relative: &Path,
        value: &Value,
        immutable: bool,
        maximum: usize,
    ) -> Result<PathBuf> {
        let body = crate::value::encoded(value)?;
        self.write(relative, &body, immutable, maximum)?;
        Ok(self.store.repo().join(relative))
    }
    pub fn write(
        &self,
        relative: &Path,
        body: &[u8],
        immutable: bool,
        maximum: usize,
    ) -> Result<()> {
        require(body.len() <= maximum, "panel artifact exceeds byte limit")?;
        self.mutate(relative, maximum, |old| {
            require(
                !immutable || old.is_empty() || old == body,
                "immutable panel artifact conflicts with existing bytes",
            )?;
            Ok(body.to_vec())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn identical_json_write_preserves_file_identity() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "oms-panel-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // An explicit mode keeps the fixture private under a group-writable umask.
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&root)
            .unwrap();
        let store = Store::open(&root).unwrap();
        let tx = store
            .transaction("snapshot-test", Duration::from_secs(2))
            .unwrap();
        let path = Path::new(".oms/panel-state/snapshot.json");
        let value = serde_json::json!({"status": "ready"});
        tx.write_json(path, &value, false, MAX_STATE).unwrap();
        let before = std::fs::metadata(root.join(path)).unwrap();
        tx.write_json(path, &value, false, MAX_STATE).unwrap();
        let after = std::fs::metadata(root.join(path)).unwrap();
        assert_eq!(
            (before.ino(), before.mtime(), before.mtime_nsec()),
            (after.ino(), after.mtime(), after.mtime_nsec())
        );
        drop(tx);

        let lock_path = Path::new("team-request-issuance.lock");
        let held = store.lock_at(lock_path, Duration::from_secs(1)).unwrap();
        let stop = AtomicBool::new(false);
        let bounds = oms_command_plan::team::AdmissionBounds {
            lock_wait: Duration::from_millis(120),
            lock_hold: Duration::from_secs(2),
        };
        let busy = store
            .lock_at_bounded(
                lock_path,
                &stop,
                Instant::now() + Duration::from_secs(5),
                bounds,
            )
            .err()
            .unwrap();
        assert_eq!(busy.code, 75);
        assert!(busy.message.contains("busy"), "{}", busy.message);

        let contender = Store::open(&root).unwrap();
        let acquired_ms = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let started = Instant::now();
                let guard = contender
                    .lock_at_bounded(
                        lock_path,
                        &stop,
                        started + Duration::from_secs(5),
                        oms_command_plan::team::AdmissionBounds {
                            lock_wait: Duration::from_secs(2),
                            lock_hold: Duration::from_secs(2),
                        },
                    )
                    .unwrap();
                guard.check().unwrap();
                started.elapsed()
            });
            std::thread::sleep(Duration::from_millis(60));
            drop(held);
            waiter.join().unwrap()
        });
        assert!(
            acquired_ms < Duration::from_millis(500),
            "bounded panel lock should poll every 20 ms rather than 1 s: {acquired_ms:?}"
        );
        let expired_hold = store
            .lock_at_bounded(
                lock_path,
                &stop,
                Instant::now() + Duration::from_secs(5),
                oms_command_plan::team::AdmissionBounds {
                    lock_wait: Duration::from_secs(1),
                    lock_hold: Duration::from_millis(10),
                },
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(expired_hold.check().unwrap_err().code, 75);
        drop(expired_hold);

        let thread_rel = Path::new(".oms/threads/panel-council-0123456789abcdef01234567.jsonl");
        let thread_abs = root.join(thread_rel);
        std::fs::create_dir_all(thread_abs.parent().unwrap()).unwrap();
        std::fs::write(&thread_abs, b"{\"seq\":0}\n").unwrap();
        let done = AtomicBool::new(false);
        let reader_steps = std::sync::atomic::AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&thread_abs)
                    .unwrap();
                barrier.wait();
                for seq in 1..=24 {
                    file.write_all(format!("{{\"seq\":{seq},\"text\":\"").as_bytes())
                        .unwrap();
                    file.flush().unwrap();
                    if seq == 1 {
                        let started = Instant::now();
                        for (idx, chunk) in b"ok\"}".chunks(1).enumerate() {
                            let target = Duration::from_micros(((idx + 1) * 700) as u64);
                            while started.elapsed() < target {
                                std::thread::yield_now();
                            }
                            file.write_all(chunk).unwrap();
                            file.flush().unwrap();
                        }
                        file.write_all(b"\n").unwrap();
                    } else {
                        file.write_all(b"ok\"}\n").unwrap();
                    }
                    file.flush().unwrap();
                    while reader_steps.load(Ordering::Acquire) < seq {
                        std::thread::yield_now();
                    }
                }
                done.store(true, Ordering::Release);
            });
            scope.spawn(|| {
                barrier.wait();
                let mut prev = 1usize;
                let mut reads = 0usize;
                while !done.load(Ordering::Acquire) || prev < 25 || reads < 24 {
                    let raw = store.read_jsonl(thread_rel, MAX_STATE, true).unwrap();
                    assert!(raw.ends_with(b"\n"));
                    let lines = raw.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count();
                    assert!(lines >= prev);
                    prev = lines;
                    reads += 1;
                    reader_steps.fetch_add(1, Ordering::Release);
                }
                assert_eq!(prev, 25);
            });
        });
        assert!(
            store
                .read_jsonl(Path::new("../escape"), MAX_STATE, true)
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
