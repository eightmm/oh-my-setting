use super::*;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

pub(super) struct Fixture {
    root: PathBuf,
    pub(super) resolver: Resolver,
}
impl Fixture {
    pub(super) fn new(name: &str) -> Self {
        let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = (0u32..)
            .find_map(|i| {
                let path = tmp.join(format!("oms-disc-{name}-{}-{i}", std::process::id()));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => Some(path),
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => None,
                    Err(e) => panic!("create test directory: {e}"),
                }
            })
            .unwrap();
        let managed_root = root.join("managed");
        fs::create_dir(&managed_root).unwrap();
        Self {
            root,
            resolver: Resolver {
                managed_root,
                inherited_room: None,
                execution_root: None,
                child: false,
            },
        }
    }
    pub(super) fn repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "initial"]);
        repo
    }
    pub(super) fn scratch(&self, origin: &Path, name: &str) -> PathBuf {
        let parent = self
            .resolver
            .managed_root
            .join(format!("oh-my-setting-scratch.{name}"));
        fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        let checkout = parent.join(format!("wt-{name}"));
        linked(origin, &checkout);
        fs::write(
            parent.join(".oh-my-setting-tmp"),
            format!(
                "kind=oh-my-setting-temp\ntemporary=1\nrepo={}\nworktree={}\n",
                origin.display(),
                checkout.display()
            ),
        )
        .unwrap();
        checkout
    }
    fn scan(&self, repo: &Path) -> Checked<ScratchDiscovery> {
        self.resolver
            .discover_scratch_checkouts(repo, &AtomicBool::new(false))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
pub(super) fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.as_bytes().starts_with(b"GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn linked(origin: &Path, checkout: &Path) {
    git(
        origin,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            checkout.to_str().unwrap(),
            "HEAD",
        ],
    );
}
fn admin(checkout: &Path) -> PathBuf {
    let raw = fs::read(checkout.join(".git")).unwrap();
    pointer(raw.strip_prefix(b"gitdir: ").unwrap())
        .unwrap()
        .to_owned()
}
#[test]
fn includes_only_verified_managed_checkout_from_this_project_and_room() {
    let mut f = Fixture::new("scope");
    let primary = f.repo("project");
    let other = f.repo("other");
    let pr = f.root.join("pr");
    linked(&primary, &pr);
    let wt = f.scratch(&pr, "active");
    f.scratch(&other, "foreign");
    let good = f.scan(&primary).unwrap();
    assert_eq!(
        (
            good.checkouts.len(),
            good.unverified,
            good.remaining,
            good.incomplete
        ),
        (1, 0, 0, None)
    );
    assert_eq!(good.checkouts[0].id().len(), 64);
    assert!(!format!("{good:?}").contains(f.root.to_str().unwrap()));
    assert_ne!(
        good.checkouts[0].id(),
        f.scan(&other).unwrap().checkouts[0].id()
    );
    assert_eq!(f.scan(&pr).unwrap_err(), OutsideScope);
    let sub = wt.join("inside");
    fs::create_dir(&sub).unwrap();
    f.resolver.inherited_room = Some(sub.clone());
    let room = f.scan(&primary).unwrap();
    assert_eq!(room.checkouts[0].id(), good.checkouts[0].id());
    fs::create_dir(sub.join(".git")).unwrap();
    assert_eq!(room.checkouts[0].revalidate(), Err(Changed));
    for foreign in [pr, other] {
        f.resolver.inherited_room = Some(foreign);
        assert_eq!(f.scan(&primary).unwrap_err(), OutsideScope);
    }
    f.resolver.child = true;
    assert_eq!(f.scan(&primary).unwrap_err(), OutsideScope);
}
#[test]
fn rejects_foreign_claim_symlink_pointer_and_ambiguous_git_registration() {
    let f = Fixture::new("pointers");
    let repo = f.repo("project");
    let other = f.repo("other");
    let a = f.scratch(&repo, "a");
    let b = f.scratch(&repo, "b");
    let marker = b.parent().unwrap().join(".oh-my-setting-tmp");
    let raw = fs::read_to_string(&marker).unwrap();
    fs::write(
        &marker,
        raw.replace(
            &format!("repo={}", repo.display()),
            &format!("repo={}", other.display()),
        ),
    )
    .unwrap();
    let report = f.scan(&repo).unwrap();
    assert_eq!(
        (report.checkouts.len(), report.unverified, report.incomplete),
        (1, 1, Some(UnverifiedRegistration))
    );
    fs::write(&marker, raw).unwrap();
    let gitdir = admin(&b);
    let indirect = f.root.join("indirect");
    symlink(&gitdir, &indirect).unwrap();
    fs::write(b.join(".git"), format!("gitdir: {}\n", indirect.display())).unwrap();
    let report = f.scan(&repo).unwrap();
    assert_eq!((report.checkouts.len(), report.unverified), (1, 1));
    fs::write(
        gitdir.join("gitdir"),
        format!("{}\n", a.join(".git").display()),
    )
    .unwrap();
    assert_eq!(f.scan(&repo).unwrap_err(), AmbiguousRegistry);
    let alias = f.root.join("duplicate");
    symlink(&a, &alias).unwrap();
    fs::write(
        gitdir.join("gitdir"),
        format!("{}\n", alias.join(".git").display()),
    )
    .unwrap();
    assert_eq!(f.scan(&repo).unwrap_err(), AmbiguousRegistry);
}
#[test]
fn retained_proof_expires_on_replacement_permissions_or_deregistration() {
    let f = Fixture::new("generation");
    let repo = f.repo("project");
    let wt = f.scratch(&repo, "one");
    let marker = wt.parent().unwrap().join(".oh-my-setting-tmp");
    let scan = f.scan(&repo).unwrap();
    let old = &scan.checkouts[0];
    old.revalidate().unwrap();
    let replacement = marker.with_extension("replacement");
    fs::write(&replacement, fs::read(&marker).unwrap()).unwrap();
    fs::rename(replacement, &marker).unwrap();
    assert_eq!(old.revalidate(), Err(Changed));
    let now = f.scan(&repo).unwrap();
    assert_ne!(old.id(), now.checkouts[0].id());
    let parent = wt.parent().unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(now.checkouts[0].revalidate(), Err(Changed));
    assert_eq!(f.scan(&repo).unwrap().unverified, 1);
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    git(&repo, &["worktree", "remove", wt.to_str().unwrap()]);
    assert_eq!(now.checkouts[0].revalidate(), Err(Changed));
    assert_eq!(f.scan(&repo).unwrap().checkouts.len(), 0);
    linked(&repo, &wt);
    let recreated = f.scan(&repo).unwrap();
    recreated.checkouts[0].revalidate().unwrap();
    assert_ne!(now.checkouts[0].id(), recreated.checkouts[0].id());
    assert_eq!(now.checkouts[0].revalidate(), Err(Changed));
}
#[test]
fn complete_snapshot_and_candidate_limits_have_distinct_results() {
    assert!(matches!(
        registry(b"worktree /one\0"),
        Err(AmbiguousRegistry)
    ));
    assert!(matches!(
        registry(b"worktree /one\0\0"),
        Err(AmbiguousRegistry)
    ));
    assert!(matches!(
        registry(&vec![0; REGISTRY_BYTES + 1]),
        Err(RegistryLimit)
    ));
    let mut raw = Vec::new();
    for i in 0..=REGISTRY_RECORDS {
        raw.extend(format!("worktree /repo-{i}\0bare\0\0").bytes());
    }
    assert!(matches!(registry(&raw), Err(RegistryLimit)));
    let parsed = registry(b"worktree /r\nx\xff\0bare\0\0").unwrap();
    assert_eq!(parsed.primary.as_os_str().as_bytes(), b"/r\nx\xff");
    let f = Fixture::new("window");
    let repo = f.repo("project");
    f.scratch(&repo, "first");
    f.scratch(&repo, "second");
    let interrupted = AtomicBool::new(false);
    let scan = Snapshot {
        deadline: Instant::now() + OBSERVATION_TIME,
        interrupted: &interrupted,
    };
    let small = f.resolver.discover_window(&repo, &scan, 1).unwrap();
    assert_eq!(
        (small.checkouts.len(), small.remaining, small.incomplete),
        (1, 1, Some(InspectionLimit))
    );
    assert_eq!(
        scan.git(&repo, &["worktree", "list", "--porcelain", "-z"], 3),
        Err(RegistryLimit)
    );
    let bounded = Snapshot {
        deadline: Instant::now() + Duration::from_millis(100),
        interrupted: &interrupted,
    };
    assert_eq!(
        bounded.git(&repo, &["-c", "alias.pause=!sleep 30", "pause"], 16),
        Err(Deadline)
    );
    let ended = Snapshot {
        deadline: Instant::now(),
        interrupted: &interrupted,
    };
    assert_eq!(
        f.resolver.discover_window(&repo, &ended, 1).unwrap_err(),
        Deadline
    );
    interrupted.store(true, Ordering::Release);
    assert_eq!(
        f.resolver.discover_window(&repo, &scan, 1).unwrap_err(),
        Interrupted
    );
}
