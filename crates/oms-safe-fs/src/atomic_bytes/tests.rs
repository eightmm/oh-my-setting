use super::*;
use crate::tests::Fixture;
use std::convert::Infallible;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

fn options() -> MutationOptions {
    MutationOptions {
        read: ByteReadOptions {
            missing_ok: true,
            max_bytes: 32,
        },
        max_output: 32,
        ..MutationOptions::default()
    }
}

fn change(
    root: &Root,
    path: &Path,
    opts: MutationOptions,
    body: &[u8],
) -> Result<MutationOutcome, MutationError<Infallible>> {
    root.atomic_mutate(path, opts, |_| Ok(body.to_vec()))
}

fn refused(error: ByteError) -> MutationError<Infallible> {
    MutationError::Storage {
        phase: MutationPhase::BeforePublication,
        error,
    }
}

fn names(path: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

fn temp_path(path: &Path) -> PathBuf {
    let name = names(path)
        .into_iter()
        .find(|name| name.as_bytes().starts_with(b".file.tmp."))
        .unwrap();
    path.join(name)
}

#[test]
fn atomic_create_replace_modes_and_raw_paths() {
    let f = Fixture::new();
    let alias = f.0.join("alias");
    std::fs::create_dir(f.0.join("repo")).unwrap();
    symlink("repo", &alias).unwrap();
    let root = Root::open(&alias).unwrap();
    let raw = OsString::from_vec(vec![b'f', 0xff]);
    let path = Path::new(&raw);
    let created = change(&root, path, options(), &[0, 0xff]);
    #[cfg(target_os = "macos")]
    if matches!(
        created,
        Err(MutationError::Storage {
            phase: MutationPhase::BeforePublication,
            error: ByteError::Io(Errno::INVAL | Errno::ILSEQ),
        })
    ) {
        return;
    }
    assert_eq!(
        created.unwrap(),
        MutationOutcome {
            bytes: vec![0, 0xff],
            disposition: MutationDisposition::Created,
        }
    );
    let real = root.path.join(path);
    let initial = std::fs::metadata(&real).unwrap();
    assert_eq!(initial.mode() & 0o777, 0o600);
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    let kept_fd = std::fs::File::open(&real).unwrap();
    assert_eq!(
        change(&root, path, options(), &[0xfe]).unwrap().disposition,
        MutationDisposition::Replaced
    );
    let after = std::fs::metadata(&real).unwrap();
    assert_ne!(after.ino(), kept_fd.metadata().unwrap().ino());
    assert_eq!(after.mode() & 0o7777, 0o640);
    assert_eq!(after.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(after.gid(), std::fs::metadata(&root.path).unwrap().gid());
    for (source_mode, expected_mode, body) in [
        (0o6640, 0o640, &[0xff, 0][..]),
        (0o7640, 0o1640, &[0xfe][..]),
    ] {
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(source_mode)).unwrap();
        assert_eq!(
            std::fs::metadata(&real).unwrap().mode() & 0o7777,
            source_mode
        );
        assert_eq!(
            change(&root, path, options(), body).unwrap().disposition,
            MutationDisposition::Replaced
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().mode() & 0o7777,
            expected_mode
        );
    }
    let logical = alias.join(path);
    assert_eq!(
        root.read_no_follow(&logical, ByteReadOptions::default()),
        Err(ByteError::InvalidPath)
    );
    assert_eq!(
        change(&root, &logical, options(), b"refused"),
        Err(refused(ByteError::InvalidPath))
    );
    assert_eq!(
        root.read_no_follow(path, ByteReadOptions::default())
            .unwrap(),
        [0xfe]
    );
    assert_eq!(
        root.read_no_follow(&real, ByteReadOptions::default())
            .unwrap(),
        [0xfe]
    );
    assert_eq!(names(&root.path), [raw]);
}

#[test]
fn atomic_missing_existence_and_parent_creation() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let path = Path::new("a/b/file");
    assert_eq!(
        root.read_no_follow(path, ByteReadOptions::default()),
        Err(ByteError::Missing)
    );
    assert_eq!(root.read_no_follow(path, options().read).unwrap(), []);
    assert_eq!(
        change(&root, path, options(), b"x"),
        Err(refused(ByteError::Missing))
    );
    let mut opts = options();
    opts.create_parent = true;
    opts.expected_exists = Some(true);
    assert_eq!(
        change(&root, path, opts, b"x"),
        Err(refused(ByteError::ExistenceMismatch))
    );
    assert_eq!(
        std::fs::metadata(f.0.join("a/b")).unwrap().mode() & 0o777,
        0o700
    );
    opts.expected_exists = Some(false);
    assert_eq!(
        change(&root, path, opts, b"").unwrap().disposition,
        MutationDisposition::Created
    );
    assert_eq!(
        change(&root, path, opts, b"x"),
        Err(refused(ByteError::ExistenceMismatch))
    );
    opts.expected_exists = Some(true);
    assert_eq!(
        change(&root, path, opts, b"x").unwrap().disposition,
        MutationDisposition::Replaced
    );
    opts.read.missing_ok = false;
    opts.expected_exists = Some(false);
    assert_eq!(
        change(&root, Path::new("absent"), opts, b"x"),
        Err(refused(ByteError::Missing))
    );
    assert!(!f.0.join("absent").exists());
}

#[test]
fn atomic_early_returns_and_transform_refusal() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let path = Path::new("file");
    std::fs::write(f.0.join(path), b"old").unwrap();
    std::fs::set_permissions(f.0.join(path), std::fs::Permissions::from_mode(0o444)).unwrap();
    let original = std::fs::metadata(f.0.join(path)).unwrap();
    let mut opts = options();
    opts.max_output = 0;
    opts.preserve_existing = true;
    let out: Result<_, MutationError<Infallible>> =
        root.atomic_mutate(path, opts, |_| panic!("preserve called transform"));
    assert_eq!(out.unwrap().disposition, MutationDisposition::Preserved);
    opts.preserve_existing = false;
    assert_eq!(
        change(&root, path, opts, b"old").unwrap().disposition,
        MutationDisposition::Unchanged
    );
    assert_eq!(
        std::fs::metadata(f.0.join(path)).unwrap().ino(),
        original.ino()
    );
    opts.expected_exists = Some(false);
    opts.preserve_existing = true;
    assert_eq!(
        change(&root, path, opts, b"old"),
        Err(refused(ByteError::ExistenceMismatch))
    );
    let result = root.atomic_mutate(path, options(), |_| Err::<Vec<u8>, _>("refused"));
    assert_eq!(result, Err(MutationError::Transform("refused")));
    assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"old");
    assert_eq!(names(&f.0), [OsString::from("file")]);
}

#[test]
fn atomic_bounds_empty_zero_equal_and_overflow() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let path = Path::new("file");
    assert_eq!(ByteReadOptions::default().max_bytes, 256 * 1024 * 1024);
    assert_eq!(MutationOptions::default().max_output, 16 * 1024 * 1024);
    let mut opts = options();
    opts.read.max_bytes = 0;
    opts.max_output = 0;
    assert_eq!(
        change(&root, path, opts, b"x"),
        Err(refused(ByteError::OutputTooLarge))
    );
    assert!(!f.0.join(path).exists());
    let (bytes, stat) = root
        .read_no_follow_checked::<ByteError>(path, opts.read, |_| panic!("absent leaf opened"))
        .unwrap();
    assert!(bytes.is_empty() && stat.is_none());
    change(&root, path, opts, b"").unwrap();
    assert_eq!(root.read_no_follow(path, opts.read).unwrap(), []);
    let mut opened = None;
    let (bytes, stat) = root
        .read_no_follow_checked::<ByteError>(path, opts.read, |stat| {
            opened = Some(*stat);
            Ok(())
        })
        .unwrap();
    let stat = stat.unwrap();
    assert!(bytes.is_empty());
    assert_eq!(stat.st_size, 0);
    assert!(unchanged(&opened.unwrap(), &stat));
    opts.max_output = 3;
    change(&root, path, opts, b"abc").unwrap();
    assert!(matches!(
        root.read_no_follow_checked(path, opts.read, |_| Err(ByteError::Unsafe)),
        Err(ByteError::Unsafe)
    ));
    assert_eq!(
        root.read_no_follow(path, opts.read),
        Err(ByteError::InputTooLarge)
    );
    assert_eq!(
        change(&root, path, opts, b"d"),
        Err(refused(ByteError::InputTooLarge))
    );
    opts.read.max_bytes = 3;
    assert_eq!(root.read_no_follow(path, opts.read).unwrap(), b"abc");
    assert_eq!(
        change(&root, path, opts, b"abcd"),
        Err(refused(ByteError::OutputTooLarge))
    );
    assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"abc");
    opts.read.max_bytes = usize::MAX;
    opts.max_output = usize::MAX;
    assert_eq!(root.read_no_follow(path, opts.read).unwrap(), b"abc");
    change(&root, path, opts, b"abcd").unwrap();
    opts.read.max_bytes = 3;
    opts.preserve_existing = true;
    assert_eq!(
        change(&root, path, opts, b""),
        Err(refused(ByteError::InputTooLarge))
    );
    assert_eq!(names(&f.0), [OsString::from("file")]);
    opts.read.max_bytes = usize::MAX;
    opts.preserve_existing = false;
    let large = vec![0xff; 65537];
    change(&root, path, opts, &large).unwrap();
    opts.read.max_bytes = large.len();
    assert_eq!(root.read_no_follow(path, opts.read).unwrap(), large);
    opts.read.max_bytes -= 1;
    assert_eq!(
        root.read_no_follow(path, opts.read),
        Err(ByteError::InputTooLarge)
    );
}

#[test]
fn atomic_paths_normalize_without_following_components() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    for path in [
        Path::new(""),
        Path::new("."),
        Path::new(".."),
        Path::new("/"),
        Path::new("a\0b"),
    ] {
        assert_eq!(
            change(&root, path, options(), b"x"),
            Err(refused(ByteError::InvalidPath))
        );
        assert_eq!(
            root.read_no_follow(path, options().read),
            Err(ByteError::InvalidPath)
        );
    }
    std::fs::create_dir(f.0.join("dir")).unwrap();
    change(&root, Path::new("dir//./../file/"), options(), b"x").unwrap();
    assert_eq!(
        root.read_no_follow(&root.path.join("file"), options().read)
            .unwrap(),
        b"x"
    );
    let other = Fixture::new();
    assert_eq!(
        change(&root, &other.0.join("file"), options(), b"x"),
        Err(refused(ByteError::InvalidPath))
    );
    symlink(&other.0, f.0.join("link")).unwrap();
    assert_eq!(
        change(&root, Path::new("link/file"), options(), b"x"),
        Err(refused(ByteError::Unsafe))
    );
    assert_eq!(
        root.read_no_follow(Path::new("link/missing"), options().read),
        Err(ByteError::Unsafe)
    );
    assert!(names(&other.0).is_empty());
    let mut opts = options();
    opts.create_parent = true;
    let deep = PathBuf::from(vec!["d"; 65].join("/")).join("file");
    change(&root, &deep, opts, b"deep").unwrap();
    assert_eq!(root.read_no_follow(&deep, opts.read).unwrap(), b"deep");
}

#[test]
fn atomic_parent_creation_failure_and_concurrent_creation() {
    for error in [None, Some(Errno::IO), Some(Errno::INVAL)] {
        let f = Fixture::new();
        let root = Root::open(&f.0).unwrap();
        let mut opts = options();
        opts.create_parent = true;
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("dir/file"),
            opts,
            |_| Ok(b"new".to_vec()),
            |stage| {
                if stage == Stage::BeforeParentSync {
                    return error.map_or(Ok(()), Err);
                }
                Ok(())
            },
        );
        assert!(f.0.join("dir").is_dir());
        if error == Some(Errno::IO) {
            assert_eq!(result, Err(refused(ByteError::Io(Errno::IO))));
            assert!(names(&f.0.join("dir")).is_empty());
        } else {
            assert_eq!(result.unwrap().disposition, MutationDisposition::Created);
        }
    }
    for unsafe_parent in [false, true] {
        let f = Fixture::new();
        let root = Root::open(&f.0).unwrap();
        let mut opts = options();
        opts.create_parent = true;
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("dir/file"),
            opts,
            |_| Ok(b"new".to_vec()),
            |stage| {
                if stage == Stage::BeforeMkdir {
                    if unsafe_parent {
                        symlink(".", f.0.join("dir")).unwrap();
                    } else {
                        std::fs::create_dir(f.0.join("dir")).unwrap();
                    }
                }
                Ok(())
            },
        );
        if unsafe_parent {
            assert_eq!(result, Err(refused(ByteError::Unsafe)));
            assert!(!f.0.join("file").exists());
        } else {
            assert_eq!(result.unwrap().disposition, MutationDisposition::Created);
        }
    }
}

#[test]
fn atomic_temp_creation_is_exclusive_and_cleanup_checks_links() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let anchor = Anchor::open(&root, Path::new("file"), false, &mut |_| Ok(())).unwrap();
    let name = OsString::from(".file.tmp.collision");
    std::fs::write(f.0.join(&name), b"unrelated").unwrap();
    assert!(matches!(
        Temp::create_named(&anchor, name.clone()),
        Err(ByteError::Io(Errno::EXIST))
    ));
    assert_eq!(std::fs::read(f.0.join(name)).unwrap(), b"unrelated");
    let temp = Temp::create(&anchor).unwrap();
    let path = f.0.join(&temp.name);
    std::fs::hard_link(&path, f.0.join("hard")).unwrap();
    drop(temp);
    assert!(path.exists());
    assert!(f.0.join("hard").exists());
}

#[test]
fn atomic_permission_failures_and_opened_root_replacement() {
    let f = Fixture::new();
    let repo = f.0.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let root = Root::open(&repo).unwrap();
    if rustix::process::geteuid().as_raw() != 0 {
        std::fs::write(repo.join("file"), b"old").unwrap();
        std::fs::set_permissions(repo.join("file"), std::fs::Permissions::from_mode(0o0)).unwrap();
        assert_eq!(
            root.read_no_follow(Path::new("file"), options().read),
            Err(ByteError::Io(Errno::ACCESS))
        );
        assert_eq!(
            change(&root, Path::new("file"), options(), b"new"),
            Err(refused(ByteError::Io(Errno::ACCESS)))
        );
        std::fs::set_permissions(repo.join("file"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut opts = options();
        opts.create_parent = true;
        let create = change(&root, Path::new("missing/child"), opts, b"x");
        let replace = change(&root, Path::new("file"), opts, b"new");
        std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(create, Err(refused(ByteError::Io(Errno::ACCESS))));
        assert_eq!(replace, Err(refused(ByteError::Io(Errno::ACCESS))));
        assert_eq!(std::fs::read(repo.join("file")).unwrap(), b"old");
    }
    std::fs::rename(&repo, f.0.join("parked")).unwrap();
    std::fs::create_dir(&repo).unwrap();
    assert_eq!(
        change(&root, Path::new("file"), options(), b"new"),
        Err(refused(ByteError::Changed))
    );
    assert!(names(&repo).is_empty());
}

#[test]
fn atomic_refuses_unsafe_leaves_ancestors_and_readonly() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let path = Path::new("file");
    std::fs::write(f.0.join(path), b"old").unwrap();
    std::fs::set_permissions(f.0.join(path), std::fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        change(&root, path, options(), b"new"),
        Err(refused(ByteError::NotWritable))
    );
    assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"old");
    std::fs::set_permissions(f.0.join(path), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(f.0.join(path), f.0.join("hard")).unwrap();
    assert_eq!(
        change(&root, path, options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    assert_eq!(
        root.read_no_follow(path, options().read),
        Err(ByteError::Unsafe)
    );
    std::fs::remove_file(f.0.join("hard")).unwrap();
    symlink(path, f.0.join("link")).unwrap();
    assert_eq!(
        change(&root, Path::new("link"), options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    std::fs::create_dir(f.0.join("directory")).unwrap();
    assert_eq!(
        change(&root, Path::new("directory"), options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    assert_eq!(
        change(&root, Path::new("file/child"), options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    assert!(
        std::process::Command::new("mkfifo")
            .arg(f.0.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        change(&root, Path::new("fifo"), options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    assert_eq!(
        root.read_no_follow(Path::new("fifo"), options().read),
        Err(ByteError::Unsafe)
    );
    let socket = std::os::unix::net::UnixListener::bind(f.0.join("socket"));
    match socket {
        Ok(_socket) => {
            assert_eq!(
                change(&root, Path::new("socket"), options(), b"new"),
                Err(refused(ByteError::Unsafe))
            );
            assert_eq!(
                root.read_no_follow(Path::new("socket"), options().read),
                Err(ByteError::Unsafe)
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("socket fixture unavailable: permission denied")
        }
        Err(error) => panic!("socket fixture failed: {error}"),
    }
    assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"old");
}

#[test]
fn atomic_owner_policy_is_not_manifest_policy() {
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let path = Path::new("file");
    std::fs::write(f.0.join(path), b"old").unwrap();
    let fd = fs::openat(&root.fd, "file", OFlags::RDONLY, Mode::empty()).unwrap();
    fs::fchown(&fd, Some(rustix::process::Uid::from_raw(12345)), None).unwrap();
    assert_eq!(root.read_no_follow(path, options().read).unwrap(), b"old");
    change(&root, path, options(), b"new").unwrap();
    assert_eq!(std::fs::metadata(f.0.join(path)).unwrap().uid(), 0);
}

#[test]
fn atomic_read_races_detect_identity_timestamp_and_ancestry_changes() {
    for stage in [Stage::BeforeLeafOpen, Stage::BeforeRead, Stage::AfterRead] {
        for action in ["replace", "rewrite", "hard", "parent", "root"] {
            let f = Fixture::new();
            let repo = f.0.join("repo");
            std::fs::create_dir_all(repo.join("dir")).unwrap();
            let target = repo.join("dir/file");
            std::fs::write(&target, b"old").unwrap();
            let root = Root::open(&repo).unwrap();
            let result = root.read_bytes_with(Path::new("dir/file"), options().read, |at| {
                if at == stage {
                    match action {
                        "replace" => {
                            std::fs::rename(&target, repo.join("old")).unwrap();
                            std::fs::write(&target, b"new").unwrap();
                        }
                        "rewrite" => std::fs::write(&target, b"changed").unwrap(),
                        "hard" => std::fs::hard_link(&target, repo.join("hard")).unwrap(),
                        "parent" => {
                            std::fs::rename(repo.join("dir"), repo.join("moved")).unwrap();
                            symlink("moved", repo.join("dir")).unwrap();
                        }
                        "root" => {
                            std::fs::rename(&repo, f.0.join("moved")).unwrap();
                            std::fs::create_dir(&repo).unwrap();
                        }
                        _ => unreachable!(),
                    }
                }
                Ok(())
            });
            assert!(
                matches!(result, Err(ByteError::Changed | ByteError::Unsafe)),
                "{action}"
            );
        }
    }
    let removed: Vec<_> = [false, true]
        .into_iter()
        .map(|missing_ok| {
            let f = Fixture::new();
            let target = f.0.join("file");
            std::fs::write(&target, b"old").unwrap();
            let root = Root::open(&f.0).unwrap();
            let mut read = options().read;
            read.missing_ok = missing_ok;
            root.read_bytes_with(Path::new("file"), read, |stage| {
                if stage == Stage::BeforeLeafOpen {
                    std::fs::remove_file(&target).unwrap();
                }
                Ok(())
            })
        })
        .collect();
    assert_eq!(removed, [Err(ByteError::Changed), Err(ByteError::Changed)]);
    let mut parent_reads = Vec::new();
    let mut parent_mutations = Vec::new();
    for missing_ok in [false, true] {
        let f = Fixture::new();
        let parent = f.0.join("dir");
        let parked = f.0.join("parked");
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("file"), b"old").unwrap();
        let root = Root::open(&f.0).unwrap();
        let mut opts = options();
        opts.read.missing_ok = missing_ok;
        let move_observed_parent = |stage| {
            if stage == Stage::BeforeDirectoryOpen {
                std::fs::rename(&parent, &parked).unwrap();
            }
            Ok(())
        };
        parent_reads.push(root.read_bytes_with(
            Path::new("dir/file"),
            opts.read,
            move_observed_parent,
        ));
        assert_eq!(std::fs::read(parked.join("file")).unwrap(), b"old");
        assert_eq!(names(&f.0), [OsString::from("parked")]);
        std::fs::rename(&parked, &parent).unwrap();
        let called = std::cell::Cell::new(false);
        parent_mutations.push(root.mutate_with(
            Path::new("dir/file"),
            opts,
            |_| {
                called.set(true);
                Ok::<_, Infallible>(b"new".to_vec())
            },
            move_observed_parent,
        ));
        assert!(!called.get());
        assert_eq!(std::fs::read(parked.join("file")).unwrap(), b"old");
        assert_eq!(names(&f.0), [OsString::from("parked")]);
    }
    assert_eq!(
        parent_reads,
        [Err(ByteError::Changed), Err(ByteError::Changed)]
    );
    assert_eq!(
        parent_mutations,
        [
            Err(refused(ByteError::Changed)),
            Err(refused(ByteError::Changed))
        ]
    );

    let race_fixture = Fixture::new();
    let race_root = Root::open(&race_fixture.0).unwrap();
    let log_path = race_fixture.0.join("race.jsonl");
    std::fs::write(&log_path, b"{\"seq\":0}\n").unwrap();
    let read_opts = ByteReadOptions {
        missing_ok: true,
        max_bytes: 1024 * 1024,
    };
    let done = std::sync::atomic::AtomicBool::new(false);
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let writer_root = Root::open(&race_fixture.0).unwrap();
            let mut opts = options();
            opts.max_output = 256;
            opts.read.max_bytes = 256;
            barrier.wait();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            for seq in 1..=32 {
                file.write_all(format!("{{\"seq\":{seq},\"body\":\"").as_bytes())
                    .unwrap();
                file.flush().unwrap();
                if seq == 1 {
                    std::thread::sleep(Duration::from_millis(12));
                }
                file.write_all(b"complete\"}\n").unwrap();
                file.flush().unwrap();
                change(
                    &writer_root,
                    Path::new("CURRENT"),
                    opts,
                    format!("race-{seq}\n").as_bytes(),
                )
                .unwrap();
            }
            done.store(true, Ordering::Release);
        });
        scope.spawn(|| {
            barrier.wait();
            let mut prev_lines = 1usize;
            let mut reads = 0usize;
            while !done.load(Ordering::Acquire) || reads < 32 {
                let (bytes, stat) = race_root
                    .read_no_follow_retrying_stat(Path::new("race.jsonl"), read_opts, true)
                    .unwrap();
                assert!(stat.is_some());
                assert!(!unfinished_jsonl(&bytes));
                let count = bytes
                    .split(|&b| b == b'\n')
                    .filter(|l| !l.is_empty())
                    .count();
                assert!(count >= prev_lines);
                prev_lines = count;
                reads += 1;
            }
            assert_eq!(prev_lines, 33);
        });
        scope.spawn(|| {
            barrier.wait();
            let mut reads = 0usize;
            while !done.load(Ordering::Acquire) || reads < 32 {
                let current = race_root
                    .read_no_follow_retrying(Path::new("CURRENT"), read_opts)
                    .unwrap();
                assert!(current.is_empty() || current.ends_with(b"\n"));
                reads += 1;
            }
        });
    });
    std::fs::write(race_fixture.0.join("crashed.jsonl"), b"{\"seq\":1").unwrap();
    let crashed = race_root
        .read_no_follow_jsonl(Path::new("crashed.jsonl"), read_opts)
        .unwrap();
    assert_eq!(crashed, b"{\"seq\":1");
    assert!(unfinished_jsonl(&crashed));
    symlink("race.jsonl", race_fixture.0.join("symlink.jsonl")).unwrap();
    let fast = std::time::Instant::now();
    assert_eq!(
        race_root.read_no_follow_jsonl(
            Path::new("absent.jsonl"),
            ByteReadOptions {
                missing_ok: false,
                max_bytes: 1024,
            },
        ),
        Err(ByteError::Missing)
    );
    assert_eq!(
        race_root.read_no_follow_jsonl(Path::new("symlink.jsonl"), read_opts),
        Err(ByteError::Unsafe)
    );
    assert_eq!(
        race_root.read_no_follow_retrying(Path::new("../escape"), read_opts),
        Err(ByteError::InvalidPath)
    );
    assert_eq!(
        race_root.read_no_follow_jsonl(
            Path::new("race.jsonl"),
            ByteReadOptions {
                missing_ok: false,
                max_bytes: 4,
            },
        ),
        Err(ByteError::InputTooLarge)
    );
    assert!(fast.elapsed() < Duration::from_millis(50));
}

#[test]
fn atomic_prepublication_failures_preserve_bytes_and_cleanup_only_owned_temp() {
    for stage in [
        Stage::BeforeProbe,
        Stage::BeforeTempOpen,
        Stage::AfterTempOpen,
        Stage::BeforeWrite,
        Stage::BeforeFileSync,
        Stage::BeforePublish,
    ] {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"old").unwrap();
        std::fs::write(f.0.join(".file.tmp.unrelated"), b"leave").unwrap();
        let root = Root::open(&f.0).unwrap();
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("file"),
            options(),
            |_| Ok(b"new".to_vec()),
            |at| {
                if at == stage { Err(Errno::IO) } else { Ok(()) }
            },
        );
        assert_eq!(result, Err(refused(ByteError::Io(Errno::IO))));
        assert_eq!(std::fs::read(f.0.join("file")).unwrap(), b"old");
        assert_eq!(
            std::fs::read(f.0.join(".file.tmp.unrelated")).unwrap(),
            b"leave"
        );
        assert_eq!(
            names(&f.0),
            [
                OsString::from(".file.tmp.unrelated"),
                OsString::from("file")
            ]
        );
    }
}

#[test]
fn atomic_replaced_temp_is_neither_published_nor_deleted() {
    for fail_early in [false, true] {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"old").unwrap();
        let root = Root::open(&f.0).unwrap();
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("file"),
            options(),
            |_| Ok(b"new".to_vec()),
            |at| {
                if at == Stage::BeforePublish {
                    let temp = temp_path(&f.0);
                    std::fs::rename(&temp, f.0.join("owned-parked")).unwrap();
                    std::fs::write(temp, b"unrelated").unwrap();
                    if fail_early {
                        return Err(Errno::IO);
                    }
                }
                Ok(())
            },
        );
        assert_eq!(
            result,
            Err(refused(if fail_early {
                ByteError::Io(Errno::IO)
            } else {
                ByteError::Changed
            }))
        );
        assert_eq!(std::fs::read(temp_path(&f.0)).unwrap(), b"unrelated");
        assert_eq!(std::fs::read(f.0.join("file")).unwrap(), b"old");
    }
}

#[test]
fn atomic_checked_mutation_refuses_names_or_body_changed_during_approval() {
    for drift in ["destination", "temp-name", "temp-body"] {
        let f = Fixture::new();
        let destination = f.0.join("file");
        let parked = f.0.join("parked");
        std::fs::write(&destination, b"old").unwrap();
        let root = Root::open(&f.0).unwrap();
        let result: Result<_, MutationError<Infallible>> = root.atomic_mutate_checked(
            Path::new("file"),
            options(),
            |_| Ok(b"new".to_vec()),
            || {
                let temp = temp_path(&f.0);
                assert_eq!(std::fs::read(&temp).unwrap(), b"new");
                match drift {
                    "destination" => {
                        std::fs::rename(&destination, &parked).unwrap();
                        std::fs::write(&destination, b"external").unwrap();
                    }
                    "temp-name" => {
                        std::fs::rename(&temp, &parked).unwrap();
                        std::fs::write(&temp, b"external").unwrap();
                    }
                    "temp-body" => std::fs::write(&temp, b"unapproved").unwrap(),
                    _ => unreachable!(),
                }
                Ok(())
            },
        );
        assert_eq!(result, Err(refused(ByteError::Changed)), "{drift}");
        if drift == "destination" {
            assert_eq!(std::fs::read(&destination).unwrap(), b"external");
            assert_eq!(std::fs::read(&parked).unwrap(), b"old");
        } else {
            assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        }
        if drift == "temp-name" {
            assert_eq!(std::fs::read(temp_path(&f.0)).unwrap(), b"external");
            assert_eq!(std::fs::read(&parked).unwrap(), b"new");
        } else {
            assert!(
                !names(&f.0)
                    .iter()
                    .any(|name| name.as_bytes().starts_with(b".file.tmp."))
            );
        }
    }
}

#[test]
fn atomic_destination_ancestor_root_and_probe_races_refuse_publication() {
    for action in [
        "replace", "rewrite", "remove", "link", "hard", "parent", "root", "probe", "appeared",
    ] {
        let f = Fixture::new();
        let repo = f.0.join("repo");
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        let target = repo.join("dir/file");
        if action != "appeared" {
            std::fs::write(&target, b"old").unwrap();
        }
        let root = Root::open(&repo).unwrap();
        let stage = if action == "probe" {
            Stage::BeforeProbe
        } else {
            Stage::BeforePublish
        };
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("dir/file"),
            options(),
            |_| Ok(b"new".to_vec()),
            |at| {
                if at == stage {
                    match action {
                        "replace" | "probe" => {
                            std::fs::rename(&target, repo.join("old")).unwrap();
                            std::fs::write(&target, b"concurrent").unwrap();
                        }
                        "appeared" => std::fs::write(&target, b"concurrent").unwrap(),
                        "rewrite" => std::fs::write(&target, b"changed").unwrap(),
                        "remove" => std::fs::remove_file(&target).unwrap(),
                        "link" => {
                            std::fs::remove_file(&target).unwrap();
                            symlink("../outside", &target).unwrap();
                        }
                        "hard" => std::fs::hard_link(&target, repo.join("hard")).unwrap(),
                        "parent" => {
                            std::fs::rename(repo.join("dir"), repo.join("moved")).unwrap();
                            std::fs::create_dir(repo.join("dir")).unwrap();
                        }
                        "root" => {
                            std::fs::rename(&repo, f.0.join("moved")).unwrap();
                            std::fs::create_dir(&repo).unwrap();
                        }
                        _ => unreachable!(),
                    }
                }
                Ok(())
            },
        );
        assert!(
            matches!(
                result,
                Err(MutationError::Storage {
                    phase: MutationPhase::BeforePublication,
                    error: ByteError::Changed | ByteError::Unsafe
                })
            ),
            "{action}: {result:?}"
        );
        if ["replace", "probe", "appeared"].contains(&action) {
            assert_eq!(std::fs::read(&target).unwrap(), b"concurrent");
        }
        let old_parent = match action {
            "root" => f.0.join("moved/dir"),
            "parent" => repo.join("moved"),
            _ => repo.join("dir"),
        };
        assert!(
            !names(&old_parent)
                .iter()
                .any(|name| name.as_bytes().starts_with(b".file.tmp."))
        );
    }
}

#[test]
fn atomic_rename_failure_and_published_fsync_ambiguity() {
    for (stage, error, phase, body, applied) in [
        (
            Stage::BeforeFileSync,
            Errno::INVAL,
            MutationPhase::BeforePublication,
            b"old",
            false,
        ),
        (
            Stage::Rename,
            Errno::IO,
            MutationPhase::PublicationUncertain,
            b"old",
            false,
        ),
        (
            Stage::Rename,
            Errno::NOENT,
            MutationPhase::PublicationUncertain,
            b"old",
            false,
        ),
        (
            Stage::Rename,
            Errno::IO,
            MutationPhase::PublicationUncertain,
            b"new",
            true,
        ),
        (
            Stage::BeforeDirectorySync,
            Errno::IO,
            MutationPhase::Published,
            b"new",
            false,
        ),
    ] {
        let f = Fixture::new();
        std::fs::write(f.0.join("file"), b"old").unwrap();
        std::fs::write(f.0.join("unrelated"), b"leave").unwrap();
        let root = Root::open(&f.0).unwrap();
        let result: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("file"),
            options(),
            |_| Ok(b"new".to_vec()),
            |at| {
                if at == stage {
                    if applied {
                        std::fs::rename(temp_path(&f.0), f.0.join("file")).unwrap();
                    }
                    Err(error)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result,
            Err(MutationError::Storage {
                phase,
                error: ByteError::Io(error)
            })
        );
        assert_eq!(std::fs::read(f.0.join("file")).unwrap(), body);
        assert_eq!(std::fs::read(f.0.join("unrelated")).unwrap(), b"leave");
        assert_eq!(
            names(&f.0),
            [OsString::from("file"), OsString::from("unrelated")]
        );
        if phase == MutationPhase::Published {
            let retried: Result<_, MutationError<Infallible>> = root.mutate_with(
                Path::new("file"),
                options(),
                |_| Ok(b"new".to_vec()),
                |at| {
                    assert!(!matches!(
                        at,
                        Stage::BeforeFileSync | Stage::BeforeDirectorySync
                    ));
                    Ok(())
                },
            );
            assert_eq!(
                retried.unwrap(),
                MutationOutcome {
                    bytes: b"new".to_vec(),
                    disposition: MutationDisposition::Unchanged,
                }
            );
        }
    }
    for error in [Errno::BADF, Errno::INVAL, Errno::NOTSUP] {
        let f = Fixture::new();
        let root = Root::open(&f.0).unwrap();
        let out: Result<_, MutationError<Infallible>> = root.mutate_with(
            Path::new("file"),
            options(),
            |_| Ok(b"new".to_vec()),
            |stage| {
                if stage == Stage::BeforeDirectorySync {
                    Err(error)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(out.unwrap().disposition, MutationDisposition::Created);
        assert_eq!(std::fs::read(f.0.join("file")).unwrap(), b"new");
    }
}

#[test]
fn atomic_overlapping_transactions_detect_second_publication() {
    let f = Fixture::new();
    std::fs::write(f.0.join("file"), b"old").unwrap();
    let root = Root::open(&f.0).unwrap();
    let follower = Root::open(&f.0).unwrap();
    let result: Result<_, MutationError<Infallible>> =
        root.atomic_mutate(Path::new("file"), options(), |old| {
            assert_eq!(old, b"old");
            change(&follower, Path::new("file"), options(), b"follower").unwrap();
            Ok(b"first".to_vec())
        });
    assert_eq!(result, Err(refused(ByteError::Changed)));
    assert_eq!(std::fs::read(f.0.join("file")).unwrap(), b"follower");
    assert_eq!(names(&f.0), [OsString::from("file")]);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn private_create_rejects_late_collision_without_overwriting() {
    let f = Fixture::new();
    let path = f.0.join("file");
    let mut root = Root::open(&f.0).unwrap();
    root.private_ancestry = Some(Vec::new());
    let result = root.mutate_with_publication(
        Path::new("file"),
        MutationOptions {
            expected_exists: Some(false),
            ..options()
        },
        |_| Ok::<_, Infallible>(b"new".to_vec()),
        None::<fn() -> Result<(), Infallible>>,
        |stage| {
            if stage == Stage::Rename {
                std::fs::write(&path, b"concurrent").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            Ok(())
        },
        true,
    );
    assert_eq!(result, Err(refused(ByteError::ExistenceMismatch)));
    assert_eq!(std::fs::read(&path).unwrap(), b"concurrent");
    assert_eq!(names(&f.0), [OsString::from("file")]);
}

#[test]
fn private_registry_byte_policy_and_mode_races() {
    let f = Fixture::new();
    std::fs::create_dir(f.0.join("dir")).unwrap();
    std::fs::set_permissions(f.0.join("dir"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = f.0.join("dir/file");
    std::fs::write(&file, b"old").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    // Exercise byte policy independently of host ancestry, which may be unsafe.
    let mut root = Root::open(&f.0).unwrap();
    root.private_ancestry = Some(Vec::new());
    for stage in [Stage::AfterRead, Stage::BeforePublish] {
        let result = root.mutate_with(
            Path::new("dir/file"),
            options(),
            |_| Ok::<_, Infallible>(b"new".to_vec()),
            |at| {
                if at == stage {
                    std::fs::set_permissions(
                        f.0.join("dir"),
                        std::fs::Permissions::from_mode(0o770),
                    )
                    .unwrap();
                }
                Ok(())
            },
        );
        assert_eq!(result, Err(refused(ByteError::Changed)));
        assert_eq!(std::fs::read(&file).unwrap(), b"old");
        std::fs::set_permissions(f.0.join("dir"), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let read = root.read_bytes_with(Path::new("dir/file"), options().read, |stage| {
        if stage == Stage::AfterRead {
            std::fs::set_permissions(f.0.join("dir"), std::fs::Permissions::from_mode(0o770))
                .unwrap();
        }
        Ok(())
    });
    assert_eq!(read, Err(ByteError::Changed));
    std::fs::set_permissions(f.0.join("dir"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        root.read_no_follow(Path::new("dir/file"), options().read),
        Err(ByteError::Unsafe)
    );
    assert_eq!(
        change(&root, Path::new("dir/file"), options(), b"new"),
        Err(refused(ByteError::Unsafe))
    );
    assert_eq!(
        Root::open(&f.0)
            .unwrap()
            .read_no_follow(Path::new("dir/file"), options().read)
            .unwrap(),
        b"old"
    );
    // Pinned metadata names one entry beneath the held root and never follows it.
    symlink(f.0.join("dir"), f.0.join("link")).unwrap();
    let (held, entry) = root.pinned_metadata(OsStr::new("link")).unwrap();
    assert_eq!(held.st_ino, std::fs::metadata(&f.0).unwrap().ino());
    assert_eq!(
        FileType::from_raw_mode(entry.unwrap().st_mode),
        FileType::Symlink
    );
    assert!(
        root.pinned_metadata(OsStr::new("absent"))
            .unwrap()
            .1
            .is_none()
    );
    for bad in ["", ".", "..", "dir/file"] {
        assert!(matches!(
            root.pinned_metadata(OsStr::new(bad)),
            Err(crate::Error::InvalidPath)
        ));
    }
}

#[test]
fn private_registry_rejects_unsafe_absolute_paths() {
    let f = Fixture::new();
    let alias = f.0.join("alias");
    symlink(&f.0, &alias).unwrap();
    assert!(Root::open_private_registry(&alias).is_err());
    assert!(Root::open(&alias).is_ok());
    assert!(matches!(
        Root::open_private_registry("/a/../b"),
        Err(crate::Error::InvalidPath)
    ));
    assert!(matches!(
        Root::open_private_registry("/a//b"),
        Err(crate::Error::InvalidPath)
    ));
}

#[test]
fn sync_parent_requires_a_safe_existing_leaf_and_reports_fsync_failure() {
    let f = Fixture::new();
    let root = Root::open(&f.0).unwrap();
    let before = names(&f.0);
    assert_eq!(root.sync_parent(Path::new("file")), Err(ByteError::Missing));
    assert_eq!(
        root.sync_parent(Path::new("absent/file")),
        Err(ByteError::Missing)
    );
    assert!(names(&f.0).is_empty());
    std::fs::create_dir(f.0.join("dir")).unwrap();
    symlink("file", f.0.join("link")).unwrap();
    for leaf in ["dir", "link"] {
        assert_eq!(root.sync_parent(Path::new(leaf)), Err(ByteError::Unsafe));
    }
    std::fs::write(f.0.join("file"), b"x").unwrap();
    assert_eq!(root.sync_parent(Path::new("file")), Ok(()));
    let failed = root.sync_parent_with(Path::new("file"), |at| {
        if at == Stage::BeforeDirectorySync {
            Err(Errno::IO)
        } else {
            Ok(())
        }
    });
    assert_eq!(failed, Err(ByteError::Io(Errno::IO)));
    assert_eq!(
        names(&f.0),
        [
            OsString::from("dir"),
            OsString::from("file"),
            OsString::from("link")
        ]
    );
    assert!(before.is_empty());
    for action in ["remove", "replace", "parent", "root"] {
        let f = Fixture::new();
        let repo = f.0.join("repo");
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        let target = repo.join("dir/file");
        std::fs::write(&target, b"old").unwrap();
        let root = Root::open(&repo).unwrap();
        let result = root.sync_parent_with(Path::new("dir/file"), |at| {
            if at == Stage::BeforeDirectorySync {
                match action {
                    "remove" => std::fs::remove_file(&target).unwrap(),
                    "replace" => {
                        std::fs::rename(&target, repo.join("old")).unwrap();
                        std::fs::write(&target, b"new").unwrap();
                    }
                    "parent" => {
                        std::fs::rename(repo.join("dir"), repo.join("moved")).unwrap();
                        std::fs::create_dir(repo.join("dir")).unwrap();
                    }
                    "root" => {
                        std::fs::rename(&repo, f.0.join("moved")).unwrap();
                        std::fs::create_dir(&repo).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            Ok(())
        });
        assert_eq!(result, Err(ByteError::Changed), "{action}");
    }
}
