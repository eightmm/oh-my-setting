use oms_lock::{Cancellation, Error, Lock, Mode, Settings, Status, legacy_seconds};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

fn write_private(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    fs::write(&path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
fn create_private_dir(path: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir(&path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-native-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn settings(&self, mode: Mode) -> Settings {
        Settings {
            mode,
            timeout: Duration::from_secs(2),
            poll_max: Duration::from_secs(1),
            lock_dir: Some(self.0.join("locks")),
            home: self.0.join("home"),
            temp_dir: self.0.clone(),
            cwd: self.0.clone(),
            search_path: std::env::var_os("PATH").unwrap_or_default(),
            force_mkdir: false,
        }
    }
    fn lock(&self, mode: Mode) -> Lock {
        Lock::for_file(&self.0.join("state"), self.settings(mode)).unwrap()
    }
    fn stale(&self, lock: &Lock, owner: &[u8], pid: &[u8], started: &[u8]) {
        fs::create_dir_all(lock.path()).unwrap();
        fs::set_permissions(
            lock.path().parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::set_permissions(lock.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in [("owner", owner), ("pid", pid), ("started", started)] {
            write_private(lock.path().join(name), bytes).unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn call_timeout() -> Duration {
    Duration::from_secs(legacy_seconds(
        std::env::var_os("OMS_TEST_CALL_TIMEOUT").as_deref(),
        30,
    ))
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + call_timeout();
    while !condition() {
        assert!(Instant::now() < deadline, "fixture condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct Process(Child);
impl Process {
    fn start(root: &Path, mode: Mode, scenario: &str) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "native_child", "--nocapture"])
            .env("OMS_NATIVE_FIXTURE", root)
            .env("OMS_NATIVE_SCENARIO", scenario)
            .env(
                "OMS_NATIVE_BACKEND",
                if mode == Mode::Flock {
                    "flock"
                } else {
                    "mkdir"
                },
            )
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0);
        Self(command.spawn().unwrap())
    }
    fn finish(&mut self) {
        let mut status = None;
        wait_for(|| {
            status = self.0.try_wait().unwrap();
            status.is_some()
        });
        assert!(status.unwrap().success());
    }
    fn crash(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn settings_bytes_and_routing() {
    for raw in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("0")),
        Some(OsStr::new("-1")),
        Some(OsStr::new("1.5")),
        Some(OsStr::new("18446744073709551616")),
    ] {
        assert_eq!(legacy_seconds(raw, 300), 300);
    }
    assert_eq!(legacy_seconds(Some(OsStr::new("001")), 300), 1);
    assert_eq!(
        legacy_seconds(Some(OsStr::new("18446744073709551615")), 300),
        u64::MAX
    );
    let f = Fixture::new();
    let mut settings = f.settings(Mode::LegacyAuto);
    settings.search_path.clear();
    assert_eq!(
        Lock::for_file(Path::new("state"), settings.clone())
            .unwrap()
            .mode(),
        Mode::Mkdir
    );
    let bin = f.0.join("bin");
    create_private_dir(&bin).unwrap();
    write_private(bin.join("flock"), b"fixture only").unwrap();
    fs::set_permissions(bin.join("flock"), fs::Permissions::from_mode(0o700)).unwrap();
    settings.search_path = bin.into_os_string();
    assert_eq!(
        Lock::for_file(Path::new("state"), settings.clone())
            .unwrap()
            .mode(),
        Mode::Flock
    );
    settings.force_mkdir = true;
    assert_eq!(
        Lock::for_file(Path::new("state"), settings.clone())
            .unwrap()
            .mode(),
        Mode::Mkdir
    );
    assert_eq!(
        Lock::for_file(Path::new(""), settings.clone()).unwrap_err(),
        Error::InvalidTarget
    );
    settings.timeout = Duration::ZERO;
    assert_eq!(
        Lock::for_file(Path::new("state"), settings).unwrap_err(),
        Error::InvalidSettings
    );
    let mut settings = f.settings(Mode::Mkdir);
    settings.poll_max = Duration::ZERO;
    assert_eq!(
        Lock::for_file(Path::new("state"), settings).unwrap_err(),
        Error::InvalidSettings
    );
    let mut settings = f.settings(Mode::Mkdir);
    settings.timeout = Duration::MAX;
    let lock = Lock::for_file(Path::new("state"), settings).unwrap();
    assert_eq!(lock.try_acquire().unwrap_err(), Error::InvalidSettings);
    assert!(!f.0.join("locks").exists());
    let bytes = OsString::from_vec(b"space \xff\nstate/".to_vec());
    let lock = Lock::for_file(Path::new(&bytes), f.settings(Mode::Mkdir)).unwrap();
    assert!(
        lock.path()
            .file_name()
            .unwrap()
            .as_bytes()
            .starts_with(b"space___state.")
    );
    assert_eq!(lock.probe(), Status::Free);
    assert!(!f.0.join("locks").exists());
    let mut settings = f.settings(Mode::Mkdir);
    settings.lock_dir = None;
    let routed = Lock::for_file(&f.0.join("state"), settings.clone()).unwrap();
    assert!(routed.path().starts_with(f.0.join(format!(
        "oh-my-setting-locks-{}",
        rustix::process::getuid().as_raw()
    ))));
    assert_eq!(routed.probe(), Status::Free);
    assert!(!routed.path().parent().unwrap().exists());
    fs::create_dir_all(&settings.home).unwrap();
    let home = Lock::for_file(&settings.home.join("state"), settings).unwrap();
    // TMPDIR is the first routing rule, even when HOME is inside it.
    assert_eq!(home.path().parent(), routed.path().parent());
}

#[test]
fn guard_lifetime_nested_callbacks_and_nonreentrancy() {
    let f = Fixture::new();
    for mode in [Mode::Mkdir, Mode::Flock] {
        let lock = f.lock(mode);
        let result = lock
            .with_lock(None, |guard| {
                guard.check().unwrap();
                assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
                let inner = Lock::for_file(&f.0.join("inner"), f.settings(mode)).unwrap();
                inner.with_lock(None, |_| 3).unwrap()
            })
            .unwrap();
        assert_eq!(result, 3);
        lock.try_acquire().unwrap().release().unwrap();
        let panic = std::panic::catch_unwind(|| lock.with_lock(None, |_| panic!("unwind fixture")));
        assert!(panic.is_err());
        lock.try_acquire().unwrap().release().unwrap();
        if mode == Mode::Flock {
            fs::remove_file(lock.path()).unwrap();
        }
    }
}

#[test]
fn stale_live_reused_and_partial_metadata() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    let live_pid = std::process::id().to_string();
    f.stale(&lock, b"live-generation", live_pid.as_bytes(), b"1");
    assert_eq!(lock.probe(), Status::Held);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
    write_private(lock.path().join("process-start"), b"different-process\n").unwrap();
    lock.try_acquire().unwrap().release().unwrap();
    f.stale(&lock, b"dead-generation", b"2147483647", b"1");
    assert_eq!(lock.probe(), Status::Free);
    lock.try_acquire().unwrap().release().unwrap();
    for (owner, pid, started) in [
        (b"partial".as_slice(), b"".as_slice(), b"".as_slice()),
        (b"", b"bad", b"1"),
        (b"partial", b"bad", b"bad"),
        (b"partial", b"0", b"1"),
    ] {
        f.stale(&lock, owner, pid, started);
        assert!(matches!(lock.probe(), Status::Held | Status::Unknown));
        assert!(lock.try_acquire().is_err());
        assert!(lock.path().is_dir());
        fs::remove_dir_all(lock.path()).unwrap();
    }
    // A generation with malformed PID and a valid aged started record is legacy-reclaimable.
    f.stale(&lock, b"partial", b"bad", b"1");
    lock.try_acquire().unwrap().release().unwrap();
    create_private_dir(lock.path()).unwrap();
    assert_eq!(lock.probe(), Status::Held);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
    File::open(lock.path())
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    assert_eq!(lock.probe(), Status::Free);
    lock.try_acquire().unwrap().release().unwrap();
}

#[test]
fn foreign_paths_metadata_limits_and_generation_release() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    fs::create_dir_all(lock.path().parent().unwrap()).unwrap();
    fs::set_permissions(
        lock.path().parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let foreign = f.0.join("foreign");
    create_private_dir(&foreign).unwrap();
    write_private(foreign.join("preserve"), b"unchanged").unwrap();
    symlink(&foreign, lock.path()).unwrap();
    assert_eq!(lock.probe(), Status::Unknown);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    fs::remove_file(lock.path()).unwrap();
    for variant in ["symlink", "hardlink", "mode", "oversize", "exact", "empty"] {
        f.stale(&lock, b"foreign", b"2147483647", b"1");
        let owner = lock.path().join("owner");
        fs::remove_file(&owner).unwrap();
        match variant {
            "symlink" => symlink(foreign.join("preserve"), &owner).unwrap(),
            "hardlink" => fs::hard_link(foreign.join("preserve"), &owner).unwrap(),
            "mode" => {
                write_private(&owner, b"foreign").unwrap();
                fs::set_permissions(&owner, fs::Permissions::from_mode(0o666)).unwrap();
            }
            "oversize" => write_private(&owner, vec![b'a'; 65537]).unwrap(),
            "exact" => write_private(&owner, vec![b'a'; 65536]).unwrap(),
            _ => write_private(&owner, b"").unwrap(),
        }
        if matches!(variant, "exact" | "empty") {
            lock.try_acquire().unwrap().release().unwrap();
        } else {
            assert!(lock.try_acquire().is_err());
            fs::remove_dir_all(lock.path()).unwrap();
        }
        assert_eq!(fs::read(foreign.join("preserve")).unwrap(), b"unchanged");
    }
    let guard = lock.try_acquire().unwrap();
    let previous = lock.path().with_extension("previous");
    fs::rename(lock.path(), &previous).unwrap();
    f.stale(
        &lock,
        b"replacement",
        std::process::id().to_string().as_bytes(),
        b"1",
    );
    assert_eq!(guard.release(), Err(Error::Changed));
    assert_eq!(fs::read(lock.path().join("owner")).unwrap(), b"replacement");
    fs::remove_dir_all(lock.path()).unwrap();
    let guard = lock.try_acquire().unwrap();
    write_private(lock.path().join("owner"), b"replacement").unwrap();
    assert_eq!(guard.release(), Err(Error::Changed));
    assert_eq!(fs::read(lock.path().join("owner")).unwrap(), b"replacement");
}

#[test]
fn moved_root_and_flock_links_refuse_without_foreign_writes() {
    let f = Fixture::new();
    for mode in [Mode::Mkdir, Mode::Flock] {
        let lock = f.lock(mode);
        let guard = lock.try_acquire().unwrap();
        let root = lock.path().parent().unwrap();
        let previous = f.0.join("previous");
        fs::rename(root, &previous).unwrap();
        create_private_dir(root).unwrap();
        assert_eq!(guard.check(), Err(Error::Changed));
        assert_eq!(
            guard.release(),
            if mode == Mode::Mkdir {
                Err(Error::Changed)
            } else {
                Ok(())
            }
        );
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
        fs::remove_dir_all(previous).unwrap();
    }
    let lock = f.lock(Mode::Flock);
    let guard = lock.try_acquire().unwrap();
    drop(guard);
    let data = f.0.join("data");
    write_private(&data, b"preserved").unwrap();
    fs::remove_file(lock.path()).unwrap();
    fs::hard_link(&data, lock.path()).unwrap();
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert_eq!(lock.probe(), Status::Unknown);
    assert_eq!(fs::read(&data).unwrap(), b"preserved");
    fs::remove_file(lock.path()).unwrap();
    symlink(&data, lock.path()).unwrap();
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert_eq!(fs::read(&data).unwrap(), b"preserved");
}

#[test]
fn timeout_and_cancellation_keep_live_authority() {
    let f = Fixture::new();
    for mode in [Mode::Mkdir, Mode::Flock] {
        let cancelled = AtomicBool::new(true);
        let untouched = Lock::at_path(
            f.0.join("uncreated").join("cancelled.lock"),
            f.settings(mode),
        )
        .unwrap();
        assert_eq!(
            untouched
                .acquire_with_cancel(|| cancelled.load(Ordering::Acquire))
                .unwrap_err(),
            Error::Cancelled
        );
        assert!(!untouched.path().parent().unwrap().exists());

        let lock = f.lock(mode);
        let guard = lock.try_acquire().unwrap();
        let cancelled = Cancellation::default();
        cancelled.cancel();
        assert_eq!(
            lock.acquire(Some(&cancelled)).unwrap_err(),
            Error::Cancelled
        );
        let mut settings = f.settings(mode);
        settings.timeout = Duration::from_millis(150);
        let contender = Lock::for_file(&f.0.join("state"), settings).unwrap();
        let start = Instant::now();
        assert_eq!(contender.acquire(None).unwrap_err(), Error::Busy);
        assert!(start.elapsed() >= Duration::from_millis(150));
        let cancellation = Cancellation::default();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| lock.acquire(Some(&cancellation)));
            cancellation.cancel();
            assert_eq!(waiter.join().unwrap().unwrap_err(), Error::Cancelled);
        });
        let interrupted = AtomicBool::new(false);
        let observed = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                lock.acquire_with_cancel(|| {
                    let cancelled = interrupted.load(Ordering::Acquire);
                    observed.store(true, Ordering::Release);
                    cancelled
                })
            });
            wait_for(|| observed.load(Ordering::Acquire));
            interrupted.store(true, Ordering::Release);
            assert_eq!(waiter.join().unwrap().unwrap_err(), Error::Cancelled);
        });
        guard.check().unwrap();
        assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
        guard.release().unwrap();
        lock.acquire_with_cancel(|| false)
            .unwrap()
            .release()
            .unwrap();
        if mode == Mode::Flock {
            fs::remove_file(lock.path()).unwrap();
        }
    }
    assert_eq!(Error::Busy.exit_code(), 75);
    assert_eq!(Error::InvalidTarget.exit_code(), 2);
}

#[test]
fn processes_recover_crash_and_never_overlap() {
    let f = Fixture::new();
    for mode in [Mode::Mkdir, Mode::Flock] {
        let lock = f.lock(mode);
        let mut holder = Process::start(&f.0, mode, "hold");
        wait_for(|| f.0.join("ready").exists());
        assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
        assert_eq!(
            lock.probe(),
            if mode == Mode::Flock && !cfg!(target_os = "linux") {
                Status::Unknown
            } else {
                Status::Held
            }
        );
        holder.crash();
        fs::remove_file(f.0.join("ready")).unwrap();
        let mut a = Process::start(&f.0, mode, "race");
        let mut b = Process::start(&f.0, mode, "race");
        a.finish();
        b.finish();
        assert_eq!(fs::read_to_string(f.0.join("count")).unwrap(), "40");
        assert!(!f.0.join("overlap").exists());
        fs::remove_file(f.0.join("count")).unwrap();
        lock.try_acquire().unwrap().release().unwrap();
        if mode == Mode::Flock {
            fs::remove_file(lock.path()).unwrap();
        }
    }
}

#[test]
#[ignore = "compiled child fixture, invoked by process tests"]
fn native_child() {
    let root = PathBuf::from(std::env::var_os("OMS_NATIVE_FIXTURE").unwrap());
    assert!(root.starts_with(std::env::temp_dir()));
    let fixture = std::mem::ManuallyDrop::new(Fixture(root));
    let mode = if std::env::var("OMS_NATIVE_BACKEND").unwrap() == "flock" {
        Mode::Flock
    } else {
        Mode::Mkdir
    };
    let mut settings = fixture.settings(mode);
    settings.timeout = call_timeout();
    let lock = Lock::for_file(&fixture.0.join("state"), settings).unwrap();
    let scenario = std::env::var("OMS_NATIVE_SCENARIO").unwrap();
    if let Some(stage) = scenario.strip_prefix("claim") {
        let stage: usize = stage.parse().unwrap();
        let mut queue_name = lock.path().as_os_str().to_owned();
        queue_name.push(".reclaim-queue");
        let queue = PathBuf::from(queue_name);
        create_private_dir(&queue)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .unwrap();
        let claim = queue.join(format!("{}.none.1.1", std::process::id()));
        create_private_dir(&claim).unwrap();
        for (name, bytes) in [
            ("choosing", b"1".as_slice()),
            ("generation", b"owner:crash"),
            ("ticket", b"1"),
        ]
        .into_iter()
        .take(stage)
        {
            write_private(claim.join(name), bytes).unwrap();
        }
        write_private(fixture.0.join("ready"), b"ready").unwrap();
        wait_for(|| fixture.0.join("release").exists());
    } else if scenario == "idle" {
        write_private(fixture.0.join("ready"), b"ready").unwrap();
        wait_for(|| fixture.0.join("release").exists());
    } else if scenario == "hold" {
        let _guard = lock.acquire(None).unwrap();
        write_private(fixture.0.join("ready"), b"ready").unwrap();
        wait_for(|| fixture.0.join("release").exists());
    } else {
        for _ in 0..20 {
            let guard = lock.acquire(None).unwrap();
            if create_private_dir(fixture.0.join("critical")).is_err() {
                write_private(fixture.0.join("overlap"), b"overlap").unwrap();
                panic!("overlap");
            }
            let path = fixture.0.join("count");
            let count = fs::read_to_string(&path)
                .map(|v| v.parse::<u32>().unwrap())
                .unwrap_or(0);
            std::thread::yield_now();
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .unwrap();
            write!(file, "{}", count + 1).unwrap();
            file.sync_all().unwrap();
            fs::remove_dir(fixture.0.join("critical")).unwrap();
            guard.release().unwrap();
        }
    }
}

#[test]
fn unexpected_stale_entry_does_not_move_the_generation() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    f.stale(&lock, b"foreign-generation", b"2147483647", b"1");
    write_private(lock.path().join("unrecognized"), b"preserve").unwrap();
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert_eq!(
        fs::read(lock.path().join("unrecognized")).unwrap(),
        b"preserve"
    );
    assert_eq!(
        fs::read(lock.path().join("owner")).unwrap(),
        b"foreign-generation"
    );
}

#[test]
fn dead_claims_live_claims_and_ticket_boundaries() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    let queue = PathBuf::from(format!("{}.reclaim-queue", lock.path().display()));
    f.stale(&lock, b"stale", b"2147483647", b"1");
    create_private_dir(&queue).unwrap();
    let dead = queue.join("2147483647.none.1.1");
    create_private_dir(&dead).unwrap();
    lock.try_acquire().unwrap().release().unwrap();
    assert!(dead.exists());
    for ticket in [b"".as_slice(), b"0", b"1", b"bad", b"18446744073709551615"] {
        f.stale(&lock, b"stale", b"2147483647", b"1");
        let claim = queue.join(format!("{}.none.1.1", std::process::id()));
        create_private_dir(&claim).unwrap();
        write_private(claim.join("choosing"), b"0").unwrap();
        write_private(claim.join("ticket"), ticket).unwrap();
        assert!(lock.try_acquire().is_err());
        assert_eq!(fs::read(lock.path().join("owner")).unwrap(), b"stale");
        fs::remove_dir_all(claim).unwrap();
        fs::remove_dir_all(lock.path()).unwrap();
    }
    f.stale(&lock, b"stale", b"2147483647", b"1");
    let unknown = queue.join("malformed");
    create_private_dir(&unknown).unwrap();
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert_eq!(fs::read(lock.path().join("owner")).unwrap(), b"stale");
}

#[test]
fn metadata_publication_crashes_stay_conservative() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    for published in 0..=4 {
        create_private_dir(lock.path().parent().unwrap())
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .unwrap();
        create_private_dir(lock.path()).unwrap();
        let fields = [
            ("owner", b"crashed-generation".as_slice()),
            ("pid", b"2147483647"),
            ("process-start", b"proc:1"),
            ("started", b"1"),
        ];
        for (name, bytes) in fields.into_iter().take(published) {
            write_private(lock.path().join(name), bytes).unwrap();
        }
        if published <= 1 {
            assert_eq!(lock.probe(), Status::Held);
            assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
            if published == 0 {
                File::open(lock.path())
                    .unwrap()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(1))
                    .unwrap();
                lock.try_acquire().unwrap().release().unwrap();
            } else {
                File::open(lock.path())
                    .unwrap()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(1))
                    .unwrap();
                assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
                fs::remove_dir_all(lock.path()).unwrap();
            }
        } else {
            lock.try_acquire().unwrap().release().unwrap();
        }
    }
}

#[test]
fn unsafe_root_and_broken_lock_probes_do_not_mutate() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    let root = lock.path().parent().unwrap();
    symlink(f.0.join("missing"), root).unwrap();
    assert_eq!(lock.probe(), Status::Unknown);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert!(!f.0.join("missing").exists());
    fs::remove_file(root).unwrap();
    create_private_dir(root).unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(lock.probe(), Status::Unknown);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(f.0.join("missing"), lock.path()).unwrap();
    assert_eq!(lock.probe(), Status::Unknown);
    assert_eq!(lock.try_acquire().unwrap_err(), Error::Unsafe);
    assert!(!f.0.join("missing").exists());
}

#[test]
fn native_thread_recovery_uses_one_pid_without_overlap() {
    let f = Fixture::new();
    let mut settings = f.settings(Mode::Mkdir);
    settings.timeout = call_timeout();
    let lock = Lock::for_file(&f.0.join("state"), settings).unwrap();
    f.stale(&lock, b"dead", b"2147483647", b"1");
    let entered = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let guard = lock.acquire(None).unwrap();
                assert_eq!(entered.fetch_add(1, Ordering::SeqCst), 0);
                std::thread::yield_now();
                assert_eq!(entered.fetch_sub(1, Ordering::SeqCst), 1);
                guard.release().unwrap();
            });
        }
    });
}

#[test]
fn exec_child_does_not_extend_parent_guard_lifetime() {
    let f = Fixture::new();
    for mode in [Mode::Mkdir, Mode::Flock] {
        let lock = f.lock(mode);
        let guard = lock.try_acquire().unwrap();
        let mut child = Process::start(&f.0, mode, "idle");
        wait_for(|| f.0.join("ready").exists());
        guard.release().unwrap();
        lock.try_acquire().unwrap().release().unwrap();
        write_private(f.0.join("release"), b"release").unwrap();
        child.finish();
        fs::remove_file(f.0.join("ready")).unwrap();
        fs::remove_file(f.0.join("release")).unwrap();
        if mode == Mode::Flock {
            fs::remove_file(lock.path()).unwrap();
        }
    }
}

#[test]
fn killed_partial_recovery_claims_do_not_wedge_generation() {
    let f = Fixture::new();
    let lock = f.lock(Mode::Mkdir);
    for stage in 0..=3 {
        f.stale(&lock, b"crash", b"2147483647", b"1");
        let mut child = Process::start(&f.0, Mode::Mkdir, &format!("claim{stage}"));
        wait_for(|| f.0.join("ready").exists());
        assert_eq!(lock.try_acquire().unwrap_err(), Error::Busy);
        child.crash();
        fs::remove_file(f.0.join("ready")).unwrap();
        lock.try_acquire().unwrap().release().unwrap();
    }
}
