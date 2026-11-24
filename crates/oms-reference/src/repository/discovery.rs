//! Read-only discovery from a single complete Git registry. Proofs identify
//! checkout generations, not tasks, live processes or completed work.
use super::{MARKER_LIMIT, Resolver, managed_scratch_parent, marker, physical_directory};
use crate::storage;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fmt, fs,
    io::{ErrorKind, Read},
    os::unix::{ffi::OsStrExt, fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const REGISTRY_BYTES: usize = 1024 * 1024;
const REGISTRY_RECORDS: usize = 4096;
const INSPECTIONS: usize = 64;
const OBSERVATION_TIME: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryIssue {
    Unavailable,
    OutsideScope,
    AmbiguousRegistry,
    RegistryLimit,
    InspectionLimit,
    UnverifiedRegistration,
    Deadline,
    Interrupted,
    Changed,
}

type Checked<T> = std::result::Result<T, DiscoveryIssue>;
use DiscoveryIssue::*;

#[derive(Debug, Default)]
pub struct ScratchDiscovery {
    pub checkouts: Vec<VerifiedScratch>,
    /// Count of examined candidates whose marker or registration was unverifiable.
    pub unverified: usize,
    /// Unchecked managed candidates after the bounded inspection window.
    pub remaining: usize,
    pub incomplete: Option<DiscoveryIssue>,
}

#[derive(Clone, Copy)]
enum CandidateOrder {
    Path,
    RecentLedger,
}

struct Snapshot<'a> {
    deadline: Instant,
    interrupted: &'a AtomicBool,
}

impl Snapshot<'_> {
    fn check(&self) -> Checked<()> {
        if self.interrupted.load(Ordering::Acquire) {
            Err(Interrupted)
        } else if Instant::now() >= self.deadline {
            Err(Deadline)
        } else {
            Ok(())
        }
    }

    fn git(&self, repo: &Path, args: &[&str], maximum: usize) -> Checked<Vec<u8>> {
        self.check()?;
        let mut cmd = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.as_bytes().starts_with(b"GIT_") {
                cmd.env_remove(key);
            }
        }
        let mut child = cmd
            .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
            .arg(repo)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| Unavailable)?;
        let mut reaped = false;
        let outcome = (|| {
            let mut pipe = child.stdout.take().ok_or(Unavailable)?;
            let flags = fcntl_getfl(&pipe).map_err(|_| Unavailable)?;
            fcntl_setfl(&pipe, flags | OFlags::NONBLOCK).map_err(|_| Unavailable)?;
            let mut out = Vec::new();
            let mut buf = [0; 8192];
            loop {
                self.check()?;
                match pipe.read(&mut buf) {
                    Ok(0) => {
                        if let Some(status) = child.try_wait().map_err(|_| Unavailable)? {
                            reaped = true;
                            return if status.success() {
                                Ok(out)
                            } else {
                                Err(Unavailable)
                            };
                        }
                    }
                    Ok(n) if out.len() + n > maximum => return Err(RegistryLimit),
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        continue;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return Err(Unavailable),
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })();
        if outcome.is_err() && !reaped {
            if let Some(group) = rustix::process::Pid::from_raw(child.id() as i32) {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        outcome
    }
}

// The primary row is meaningful only in an unambiguous, fully terminated registry.
struct Registry {
    primary: PathBuf,
    paths: BTreeSet<PathBuf>,
}

fn registry(raw: &[u8]) -> Checked<Registry> {
    if raw.len() > REGISTRY_BYTES {
        return Err(RegistryLimit);
    }
    let fields = raw.strip_suffix(b"\0").ok_or(AmbiguousRegistry)?;
    let mut lines = fields.split(|b| *b == 0);
    let mut primary = None;
    let mut paths = BTreeSet::new();
    while let Some(field) = lines.next() {
        if paths.len() == REGISTRY_RECORDS {
            return Err(RegistryLimit);
        }
        let path = Path::new(OsStr::from_bytes(
            field.strip_prefix(b"worktree ").ok_or(AmbiguousRegistry)?,
        ));
        if !path.is_absolute()
            || path
                .components()
                .any(|p| p == std::path::Component::ParentDir)
            || !paths.insert(path.to_owned())
        {
            return Err(AmbiguousRegistry);
        }
        if primary.is_none() {
            primary = Some(path.to_owned());
        }
        let (mut head, mut kind) = (false, None);
        loop {
            match lines.next() {
                Some([]) if (head && kind == Some(false)) || (!head && kind == Some(true)) => break,
                Some(field) if field.starts_with(b"HEAD ") => {
                    let oid = &field[5..];
                    if head
                        || !matches!(oid.len(), 40 | 64)
                        || !oid.iter().all(u8::is_ascii_hexdigit)
                    {
                        return Err(AmbiguousRegistry);
                    }
                    head = true;
                }
                Some(field)
                    if field == b"bare"
                        || field == b"detached"
                        || field
                            .strip_prefix(b"branch ")
                            .is_some_and(|name| !name.is_empty()) =>
                {
                    if kind.replace(field == b"bare").is_some() {
                        return Err(AmbiguousRegistry);
                    }
                }
                Some(field) if !field.is_empty() && !field.starts_with(b"worktree ") => {}
                _ => return Err(AmbiguousRegistry),
            }
        }
    }
    Ok(Registry {
        primary: primary.ok_or(AmbiguousRegistry)?,
        paths,
    })
}

fn physical(path: &Path) -> Checked<PathBuf> {
    let result = physical_directory(path).map_err(|_| Changed)?;
    if result.as_os_str() != path.as_os_str() {
        return Err(Changed);
    }
    Ok(result)
}

fn pointer(raw: &[u8]) -> Checked<&Path> {
    let raw = raw.strip_suffix(b"\n").ok_or(Changed)?;
    if raw.is_empty() || raw.len() > 4096 || raw.contains(&0) {
        return Err(Changed);
    }
    Ok(Path::new(OsStr::from_bytes(raw)))
}

#[derive(Default)]
struct Proof {
    dirs: Vec<(PathBuf, fs::Metadata)>,
    files: Vec<(PathBuf, fs::Metadata)>,
    absent: Vec<PathBuf>,
    private: Option<PathBuf>,
}

impl Proof {
    fn dir(&mut self, path: &Path) -> Checked<()> {
        physical(path)?;
        let info = fs::symlink_metadata(path).map_err(|_| Changed)?;
        if !info.is_dir()
            || info.uid() != rustix::process::getuid().as_raw()
            || info.mode() & 0o002 != 0
        {
            return Err(Changed);
        }
        self.dirs.push((path.to_owned(), info));
        Ok(())
    }
    /// Pin the nearest existing physical parent and the first missing name.
    /// An absent scratch namespace can then be distinguished from redirection,
    /// denied access or a registry entry that outlived its checkout.
    fn managed_root(&mut self, configured: &Path, scan: &Snapshot<'_>) -> Checked<(PathBuf, bool)> {
        let managed: PathBuf = std::path::absolute(configured)
            .map_err(|_| Unavailable)?
            .components()
            .collect();
        if managed
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(Unavailable);
        }
        let mut first_missing = None;
        for parent in managed.ancestors() {
            scan.check()?;
            match fs::symlink_metadata(parent) {
                Ok(_) => {
                    self.dir(parent)?;
                    let present = first_missing.is_none();
                    if let Some(path) = first_missing {
                        self.absent.push(path);
                    }
                    return Ok((managed, present));
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    first_missing = Some(parent.to_owned())
                }
                Err(_) => return Err(Unavailable),
            }
        }
        Err(Unavailable)
    }
    fn file(&mut self, path: &Path) -> Checked<Vec<u8>> {
        let (raw, info) = storage::regular(path, MARKER_LIMIT, 1).map_err(|_| Changed)?;
        self.files.push((path.to_owned(), info));
        Ok(raw)
    }
    fn pointer_to(&mut self, from: &Path, raw: &[u8]) -> Checked<PathBuf> {
        let raw = pointer(raw)?;
        let mut current = if raw.is_absolute() {
            PathBuf::from("/")
        } else {
            from.to_owned()
        };
        let mut named = false;
        for (i, component) in raw.components().enumerate() {
            if i >= 128 {
                return Err(Changed);
            }
            match component {
                std::path::Component::RootDir | std::path::Component::CurDir => {}
                std::path::Component::Normal(part) => {
                    current.push(part);
                    named = true;
                }
                std::path::Component::ParentDir => {
                    if named {
                        return Err(Changed);
                    }
                    self.dir(&current)?;
                    if !current.pop() {
                        return Err(Changed);
                    }
                }
                _ => return Err(Changed),
            }
        }
        physical(&current)
    }

    fn linked(&mut self, checkout: &Path, common: &Path) -> Checked<PathBuf> {
        self.dir(checkout)?;
        let worktrees = common.join("worktrees");
        self.dir(&worktrees)?;
        let gitfile = checkout.join(".git");
        let forward = self.file(&gitfile)?;
        let admin = self.pointer_to(checkout, forward.strip_prefix(b"gitdir: ").ok_or(Changed)?)?;
        if admin.parent() != Some(&worktrees) {
            return Err(Changed);
        }
        self.dir(&admin)?;
        let back = self.file(&admin.join("gitdir"))?;
        if pointer(&back)?.as_os_str() != gitfile.as_os_str() {
            return Err(Changed);
        }
        let relative = self.file(&admin.join("commondir"))?;
        if self.pointer_to(&admin, &relative)? != common {
            return Err(Changed);
        }
        Ok(admin)
    }
    fn check(&self) -> Checked<()> {
        if let Some(parent) = &self.private {
            storage::private_directory(parent).map_err(|_| Changed)?;
        }
        for (path, old) in &self.dirs {
            physical(path)?;
            let new = fs::symlink_metadata(path).map_err(|_| Changed)?;
            if !storage::same(old, &new) || old.uid() != new.uid() || old.mode() != new.mode() {
                return Err(Changed);
            }
        }
        for path in &self.absent {
            if !fs::symlink_metadata(path).is_err_and(|e| e.kind() == ErrorKind::NotFound) {
                return Err(Changed);
            }
        }
        for (path, old) in &self.files {
            let new = fs::symlink_metadata(path).map_err(|_| Changed)?;
            if !storage::generation(old, &new) {
                return Err(Changed);
            }
        }
        Ok(())
    }
}

/// An opaque, generation-bound identity. It carries no lifecycle status and
/// its diagnostic representation does not disclose local repository paths.
pub struct VerifiedScratch {
    id: String,
    binding: Arc<Proof>,
    registration: Proof,
    checkout: PathBuf,
    git_dir: PathBuf,
}
impl fmt::Debug for VerifiedScratch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedScratch")
            .field("id", &self.id)
            .finish()
    }
}
impl VerifiedScratch {
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Consumers can reject removal or replacement after the initial observation.
    pub fn revalidate(&self) -> Checked<()> {
        self.binding.check()?;
        self.registration.check()
    }
}

fn registration(
    primary: &Path,
    common: &Path,
    managed: &Path,
    registered: &BTreeSet<PathBuf>,
    checkout: &Path,
) -> Checked<(Proof, String, PathBuf)> {
    let parent = managed_scratch_parent(checkout).ok_or(Changed)?;
    if parent.parent() != Some(managed) || !registered.contains(checkout) {
        return Err(Changed);
    }
    storage::private_directory(&parent).map_err(|_| Changed)?;
    let mut proof = Proof {
        private: Some(parent.clone()),
        ..Proof::default()
    };
    proof.dir(&parent)?;
    let marker_path = parent.join(".oh-my-setting-tmp");
    let (fields, meta) = marker(&marker_path).map_err(|_| Changed)?;
    let intended = Path::new(&fields["worktree"]);
    let origin = Path::new(&fields["repo"]);
    if intended.as_os_str() != checkout.as_os_str() || !registered.contains(origin) {
        return Err(Changed);
    }
    if origin != primary {
        proof.linked(origin, common)?;
    }
    let git_dir = proof.linked(checkout, common)?;
    let mut hash = Sha256::new();
    hash.update(b"oms-verified-scratch-v1\0");
    hash.update(primary.as_os_str().as_bytes());
    hash.update(b"\0");
    hash.update(checkout.as_os_str().as_bytes());
    proof.files.push((marker_path, meta));
    for (_, info) in &proof.dirs {
        hash.update(info.dev().to_be_bytes());
        hash.update(info.ino().to_be_bytes());
    }
    for (_, info) in &proof.files {
        for value in [
            info.dev(),
            info.ino(),
            info.ctime() as u64,
            info.ctime_nsec() as u64,
        ] {
            hash.update(value.to_be_bytes());
        }
    }
    proof.check()?;
    Ok((proof, format!("{:x}", hash.finalize()), git_dir))
}

impl Resolver {
    /// Observe a canonical shared repository. A linked project that resolves
    /// to a different shared root cannot claim the primary project's checkouts.
    pub fn discover_scratch_checkouts(
        &self,
        shared: &Path,
        interrupted: &AtomicBool,
    ) -> Checked<ScratchDiscovery> {
        self.discover_window(
            shared,
            &Snapshot {
                deadline: Instant::now() + OBSERVATION_TIME,
                interrupted,
            },
            INSPECTIONS,
        )
    }

    fn discover_window(
        &self,
        shared: &Path,
        scan: &Snapshot<'_>,
        window: usize,
    ) -> Checked<ScratchDiscovery> {
        self.discover_sources(shared, scan, window, CandidateOrder::Path)
            .map(|(discovery, _, _)| discovery)
    }
    fn discover_sources(
        &self,
        shared: &Path,
        scan: &Snapshot<'_>,
        window: usize,
        order: CandidateOrder,
    ) -> Checked<(ScratchDiscovery, Arc<Proof>, PathBuf)> {
        scan.check()?;
        if self.child {
            return Err(OutsideScope);
        }
        let primary = physical(shared).map_err(|_| OutsideScope)?;
        let raw = scan.git(&primary, &["rev-parse", "--absolute-git-dir"], MARKER_LIMIT)?;
        let common = physical(pointer(&raw)?)?;
        let mut binding = Proof::default();
        binding.dir(&primary)?;
        binding.dir(&common)?;
        let main_git = primary.join(".git");
        if main_git != common {
            let forward = binding.file(&main_git)?;
            let resolved = binding.pointer_to(
                &primary,
                forward.strip_prefix(b"gitdir: ").ok_or(OutsideScope)?,
            )?;
            if resolved != common {
                return Err(OutsideScope);
            }
        }
        let top = scan.git(
            &primary,
            &["rev-parse", "--is-bare-repository", "--show-toplevel"],
            MARKER_LIMIT,
        )?;
        if pointer(top.strip_prefix(b"false\n").ok_or(OutsideScope)?)?.as_os_str()
            != primary.as_os_str()
        {
            return Err(OutsideScope);
        }
        let raw = scan.git(
            &primary,
            &["worktree", "list", "--porcelain", "-z"],
            REGISTRY_BYTES,
        )?;
        let registry = registry(&raw)?;
        if registry.primary.as_os_str() != primary.as_os_str() {
            return Err(OutsideScope);
        }
        // Git reports the name in each admin back-pointer verbatim. Alternate
        // spellings of a present checkout must not turn duplicate registrations
        // into separate authenticated identities.
        let mut physical_rows = BTreeSet::new();
        for row in &registry.paths {
            scan.check()?;
            match fs::canonicalize(row) {
                Ok(resolved) => {
                    if !physical_rows.insert(resolved) {
                        return Err(AmbiguousRegistry);
                    }
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(_) => return Err(Unavailable),
            }
        }
        binding.check()?;
        let (managed, present) = binding.managed_root(&self.managed_root, scan)?;
        let mut room_proof = None;
        if let Some(inherited) = &self.inherited_room {
            // Pin the physical Room locator itself. Alias retargeting must not
            // carry an inherited authorization onto another repository.
            let room = physical(inherited).map_err(|_| OutsideScope)?;
            let top = scan.git(&room, &["rev-parse", "--show-toplevel"], MARKER_LIMIT)?;
            let top = physical(pointer(&top)?)?;
            if !room.starts_with(&top) {
                return Err(OutsideScope);
            }
            for (depth, ancestor) in room.ancestors().take_while(|path| *path != top).enumerate() {
                if depth >= 128 {
                    return Err(OutsideScope);
                }
                binding.dir(ancestor)?;
                binding.absent.push(ancestor.join(".git"));
            }
            if top != primary {
                if !present {
                    return Err(OutsideScope);
                }
                room_proof = Some(
                    registration(&primary, &common, &managed, &registry.paths, &top)
                        .map_err(|_| OutsideScope)?
                        .0,
                );
            }
        }
        if let Some(room) = room_proof {
            binding.dirs.extend(room.dirs);
            binding.files.extend(room.files);
            binding.private = room.private;
        }
        binding.check()?;
        let binding = Arc::new(binding);
        let mut candidates: Vec<_> = registry
            .paths
            .iter()
            .filter(|path| {
                managed_scratch_parent(path).is_some_and(|parent| parent.parent() == Some(&managed))
            })
            .collect();
        if !present {
            binding.check()?;
            scan.check()?;
            let unverified = candidates.len();
            let report = ScratchDiscovery {
                unverified,
                incomplete: (unverified > 0).then_some(UnverifiedRegistration),
                ..ScratchDiscovery::default()
            };
            return Ok((report, binding, common));
        }
        if matches!(order, CandidateOrder::RecentLedger) {
            lifecycle::prioritize(&mut candidates, &managed, &binding, scan)?;
        }
        let mut candidates = candidates.into_iter();
        let mut out = ScratchDiscovery::default();
        while let Some(path) = candidates.next() {
            let reason = match scan.check() {
                Err(Interrupted) => return Err(Interrupted),
                Err(e) => Some(e),
                Ok(()) if out.checkouts.len() + out.unverified >= window.min(INSPECTIONS) => {
                    Some(InspectionLimit)
                }
                Ok(()) => None,
            };
            if let Some(reason) = reason {
                out.incomplete = Some(reason);
                out.remaining = 1 + candidates.count();
                break;
            }
            match registration(&primary, &common, &managed, &registry.paths, path) {
                Ok((registration, id, git_dir)) => out.checkouts.push(VerifiedScratch {
                    id,
                    checkout: path.to_owned(),
                    git_dir,
                    binding: Arc::clone(&binding),
                    registration,
                }),
                Err(_) => out.unverified += 1,
            }
        }
        binding.check()?;
        if let Err(reason) = scan.check() {
            if reason == Interrupted {
                return Err(reason);
            }
            out.incomplete = Some(reason);
        }
        if out.incomplete.is_none() && out.unverified > 0 {
            out.incomplete = Some(UnverifiedRegistration);
        }
        Ok((out, binding, common))
    }
}

mod lifecycle;
pub use lifecycle::{
    AttemptHead, LifecycleObservation, LifecycleScope, LifecycleSource, ObservedAttempt,
    SourceKind, SourceStatus,
};

#[cfg(test)]
mod tests;
