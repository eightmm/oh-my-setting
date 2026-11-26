use crate::{
    Context, Error, Output, Result, V,
    cli::Args,
    fail, json, now, obj, out, peer, process,
    providers::{self, Target},
    safety, storage,
};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

struct CargoSlot {
    _guard: oms_lock::Guard,
    target_dir: PathBuf,
}

#[must_use = "slot lock must outlive processes running in the temporary worktree"]
pub(crate) struct CargoTargetGuard {
    _slot: Option<CargoSlot>,
}

const CARGO_SLOTS: usize = 12;

fn is_cargo_repo(repo: &Path) -> bool {
    repo.join("Cargo.toml").is_file() || repo.join("rust/Cargo.toml").is_file()
}

fn cargo_slot_opted_out() -> bool {
    std::env::var_os("OMS_DELEGATE_ISOLATED_BUILD").as_deref() == Some(OsStr::new("1"))
        || std::env::var_os("OMS_CARGO_SLOT_CACHE").as_deref() == Some(OsStr::new("0"))
}

fn acquire_cargo_slot(repo: &Path, tree: &Path, provider: &str) -> Option<CargoSlot> {
    if !(is_cargo_repo(repo) || is_cargo_repo(tree)) || cargo_slot_opted_out() {
        return None;
    }
    let key = storage::hash(repo.to_string_lossy().as_bytes());
    let base = if provider == "codex" {
        std::env::temp_dir()
            .join(format!(
                "oms-cargo-slots-{}/cargo-slots",
                rustix::process::geteuid().as_raw()
            ))
            .join(&key[..16])
    } else {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })?;
        cache_root
            .join("oh-my-setting/cargo-slots")
            .join(&key[..16])
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(&base).ok()?;
    let mut settings = oms_lock::Settings::from_env().ok()?;
    settings.cwd = repo.to_owned();
    for slot in 0..CARGO_SLOTS {
        let slot_dir = base.join(format!("slot-{slot}"));
        let _ = builder.create(&slot_dir);
        let lock = oms_lock::Lock::for_file(&slot_dir.join("slot.lock"), settings.clone()).ok()?;
        if let Ok(guard) = lock.try_acquire() {
            let target_dir = slot_dir.join("target");
            if builder.create(&target_dir).is_ok() {
                return Some(CargoSlot {
                    _guard: guard,
                    target_dir,
                });
            }
        }
    }
    None
}

pub(crate) struct Worktree {
    pub path: PathBuf,
    pub base: String,
    pub owner: PathBuf,
}

#[derive(PartialEq, Eq)]
struct GitFileGeneration {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    protection: (u32, u32, u32, u64),
}
impl GitFileGeneration {
    fn read(meta: &std::fs::Metadata) -> Option<Self> {
        (meta.is_file() && meta.nlink() == 1).then(|| Self {
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            protection: (meta.mode(), meta.uid(), meta.gid(), meta.nlink()),
        })
    }

    fn same_file(&self, other: &Self) -> bool {
        (self.device, self.inode, self.size, self.protection)
            == (other.device, other.inode, other.size, other.protection)
    }
}

struct GitFileWitness {
    path: PathBuf,
    opened: Option<(std::fs::File, GitFileGeneration)>,
}
impl GitFileWitness {
    fn open(path: PathBuf, optional: bool) -> Result<Self> {
        use rustix::fs::OFlags;
        let opened = match std::fs::File::options()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
            .open(&path)
        {
            Ok(file) => {
                let generation =
                    GitFileGeneration::read(&file.metadata()?).ok_or_else(worktree_git_changed)?;
                Some((file, generation))
            }
            Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let witness = Self { path, opened };
        witness.recheck()?;
        Ok(witness)
    }

    fn recheck(&self) -> Result<()> {
        match (&self.opened, self.path.symlink_metadata()) {
            (Some((file, expected)), Ok(named))
                if GitFileGeneration::read(&named).as_ref() == Some(expected)
                    && GitFileGeneration::read(&file.metadata()?).as_ref() == Some(expected)
                    && self.path.canonicalize()? == self.path =>
            {
                Ok(())
            }
            (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(worktree_git_changed()),
        }
    }

    fn digest(&self, check: &impl Fn() -> Result<()>) -> Result<([u8; 32], GitFileGeneration)> {
        const LIMIT: u64 = 64 * 1024 * 1024;
        let (file, expected) = self.opened.as_ref().ok_or_else(worktree_git_changed)?;
        let stamp = || GitFileGeneration::read(&file.metadata()?).ok_or_else(worktree_git_changed);
        let before = stamp()?;
        if !before.same_file(expected) || before.size > LIMIT {
            return Err(worktree_git_changed());
        }
        let mut reader = file.try_clone()?;
        reader.seek(SeekFrom::Start(0))?;
        let mut reader = reader.take(LIMIT + 1);
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            check()?;
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        if LIMIT + 1 - reader.limit() != before.size || stamp()? != before {
            return Err(worktree_git_changed());
        }
        Ok((digest.finalize().into(), before))
    }
}

fn worktree_git_changed() -> Error {
    Error::new(
        2,
        "Plan worker Git generation changed before Start; preserved for inspection",
    )
}

pub(crate) struct StartCheckout {
    directory: (u64, u64),
    admin: PathBuf,
    admin_generation: (u64, u64),
    files: Vec<GitFileWitness>,
}

fn physical_directory_generation(path: &Path) -> Result<(u64, u64)> {
    let info = path.symlink_metadata()?;
    if !info.is_dir() || path.canonicalize()? != path {
        return fail("Plan worker checkout does not have its expected physical path");
    }
    Ok((info.dev(), info.ino()))
}

fn git_output_path(bytes: &[u8]) -> Result<&Path> {
    let name = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if name.is_empty() || name.contains(&b'\r') || name.contains(&b'\n') || name.contains(&0) {
        return Err(worktree_git_changed());
    }
    Ok(Path::new(OsStr::from_bytes(name)))
}
impl Worktree {
    pub(crate) fn create(repo: &Path, base: &str, context: &Context<'_>) -> Result<Self> {
        if base.is_empty() || base.starts_with('-') {
            return fail("invalid worktree base");
        }
        let resolved = process::git(
            context,
            repo,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )?;
        let base = String::from_utf8_lossy(&resolved).trim().to_owned();
        let name = crate::id("peer-")?;
        let directory = Path::new(".oms/tmp").join(&name);
        let owner_path = directory.join("owner.json");
        let _owner_guard = storage::lock(repo, &owner_path, context.interrupted)?;
        storage::write_locked(
            repo,
            &owner_path,
            &_owner_guard,
            json(&obj([
                ("schema", 1usize.into()),
                ("base", base.clone().into()),
                ("id", name.into()),
            ]))?,
            false,
            context.interrupted,
        )?;
        let path = repo.join(directory).join("tree");
        let value = path
            .to_str()
            .ok_or_else(|| Error::new(2, "worktree path is not UTF-8"))?;
        process::git(
            context,
            repo,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "--detach",
                value,
                &base,
            ],
        )?;
        Ok(Self {
            path,
            base,
            owner: repo.into(),
        })
    }
    pub(crate) fn identity(&self, context: &Context<'_>) -> Result<()> {
        Self::check_identity(&self.path, &self.base, &|args| {
            process::git(context, &self.path, args)
        })
    }
    fn check_identity(
        path: &Path,
        base: &str,
        git: &impl Fn(&[&str]) -> Result<Vec<u8>>,
    ) -> Result<()> {
        let found = git(&["rev-parse", "--show-toplevel"])?;
        if Path::new(String::from_utf8_lossy(&found).trim()).canonicalize()?
            != path.canonicalize()?
        {
            return fail("worker worktree identity changed; preserved for inspection");
        }
        let head = git(&["rev-parse", "HEAD"])?;
        if String::from_utf8_lossy(&head).trim() != base {
            return fail("worker changed HEAD; worktree preserved for inspection");
        }
        Ok(())
    }
    pub(crate) fn check_bound_base(
        path: &Path,
        base: &str,
        git: impl Fn(&[&str]) -> Result<Vec<u8>>,
    ) -> Result<()> {
        Self::check_identity(path, base, &git)?;
        let staged = git(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--ignore-submodules=none",
            "--name-only",
            "-z",
            base,
            "--",
        ])?;
        if !staged.is_empty() {
            return fail(
                "Plan worker index differs from its issued base; worktree preserved for inspection",
            );
        }
        Ok(())
    }
    pub(crate) fn bound_start_generation(&self) -> Result<(u64, u64)> {
        physical_directory_generation(&self.path)
    }

    pub(crate) fn prove_start_checkout(
        &self,
        generation: (u64, u64),
        base: &str,
        git: impl Fn(&[&str]) -> Result<Vec<u8>>,
        check: impl Fn() -> Result<()>,
    ) -> Result<StartCheckout> {
        check()?;
        if self.bound_start_generation()? != generation {
            return Err(worktree_git_changed());
        }
        let link = GitFileWitness::open(self.path.join(".git"), false)?;
        let output = git(&["rev-parse", "--absolute-git-dir"])?;
        let admin = git_output_path(&output)?.to_owned();
        let admin_generation = physical_directory_generation(&admin)?;
        let head = GitFileWitness::open(admin.join("HEAD"), false)?;
        let mut head_bytes = Vec::new();
        (&head.opened.as_ref().ok_or_else(worktree_git_changed)?.0)
            .take(66)
            .read_to_end(&mut head_bytes)?;
        if head_bytes.strip_suffix(b"\n") != Some(base.as_bytes()) {
            return Err(worktree_git_changed());
        }
        let index = GitFileWitness::open(admin.join("index"), true)?;
        let shared = git(&["rev-parse", "--shared-index-path"])?;
        let shared = if shared.is_empty() {
            None
        } else {
            let named = git_output_path(&shared)?;
            let name = named.file_name().ok_or_else(worktree_git_changed)?;
            let path = admin.join(name);
            if !name.as_bytes().starts_with(b"sharedindex.")
                || self.path.join(named).canonicalize()? != path
            {
                return Err(worktree_git_changed());
            }
            let file = GitFileWitness::open(path, false)?;
            let (digest, _) = file.digest(&check)?;
            Some((file, digest))
        };
        Self::check_bound_base(&self.path, base, git)?;
        let mut files = vec![link, head, index];
        if let Some((mut file, expected)) = shared {
            // Git freshens a split index's timestamp even on a read. Accept
            // that change only for unchanged content through the same inode.
            let (actual, generation) = file.digest(&check)?;
            if actual != expected {
                return Err(worktree_git_changed());
            }
            file.opened.as_mut().ok_or_else(worktree_git_changed)?.1 = generation;
            file.recheck()?;
            files.push(file);
        }
        Ok(StartCheckout {
            directory: generation,
            admin,
            admin_generation,
            files,
        })
    }

    pub(crate) fn recheck_start_checkout(&self, witness: &StartCheckout) -> Result<()> {
        if self.bound_start_generation()? != witness.directory
            || physical_directory_generation(&witness.admin)? != witness.admin_generation
        {
            return Err(worktree_git_changed());
        }
        for file in &witness.files {
            file.recheck()?;
        }
        Ok(())
    }
    fn staged_patch(&self, context: &Context<'_>) -> Result<Vec<u8>> {
        process::git(
            context,
            &self.path,
            &[
                "-c",
                "diff.external=",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--cached",
                &self.base,
                "--",
            ],
        )
    }

    fn prepare_bound_repair(&self, captured: &[u8], context: &Context<'_>) -> Result<()> {
        self.identity(context)?;
        if self.staged_patch(context)? != captured {
            return fail(
                "Plan worker index changed after patch capture; worktree preserved for inspection",
            );
        }
        // Keep the captured edits while handing the next invocation a base index.
        process::git(context, &self.path, &["read-tree", "--reset", &self.base])?;
        Ok(())
    }

    fn pristine(&self, context: &Context<'_>) -> Result<()> {
        self.identity(context)?;
        let state = process::git(
            context,
            &self.path,
            &[
                "-c",
                "core.fsmonitor=false",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ],
        )?;
        if !state.is_empty() {
            return fail("context snapshot changed; worktree preserved for inspection");
        }
        Ok(())
    }
    pub(crate) fn remove(self, context: &Context<'_>) -> Result<()> {
        self.identity(context)?;
        let path = self
            .path
            .to_str()
            .ok_or_else(|| Error::new(2, "invalid worktree path"))?;
        process::git(
            context,
            &self.owner,
            &["worktree", "remove", "--force", path],
        )?;
        self.reclaim_wrapper();
        Ok(())
    }
    /// Best effort after a clean removal: drop `owner.json` and the emptied
    /// `.oms/tmp/peer-<id>` wrapper using descriptor-anchored `O_NOFOLLOW`
    /// handles. Any symlink or unexpected entry leaves the wrapper untouched.
    fn reclaim_wrapper(&self) {
        use rustix::fs::{
            AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat,
        };
        let Some(wrapper) = self.path.parent() else {
            return;
        };
        let Some(id) = wrapper.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if !id.starts_with("peer-")
            || wrapper.parent() != Some(self.owner.join(".oms/tmp").as_path())
        {
            return;
        }
        let Ok(oms) = open(
            &self.owner,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .and_then(|root_fd| {
            openat(
                &root_fd,
                ".oms",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
        .and_then(|oms_fd| {
            openat(
                &oms_fd,
                "tmp",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        }) else {
            return;
        };
        let Ok(before) = statat(&oms, id, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return;
        }
        let Ok(wrapper_fd) = openat(
            &oms,
            id,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return;
        };
        let Ok(opened) = fstat(&wrapper_fd) else {
            return;
        };
        if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
            return;
        }
        let Ok(stream) = Dir::read_from(&wrapper_fd) else {
            return;
        };
        let mut names = Vec::new();
        for entry in stream {
            let Ok(entry) = entry else { return };
            let name = entry.file_name();
            if [b".".as_slice(), b"..".as_slice()].contains(&name.to_bytes()) {
                continue;
            }
            names.push(name.to_owned());
        }
        let owner_present = names.iter().any(|n| n.as_bytes() == b"owner.json");
        if names.len() > usize::from(owner_present) {
            return;
        }
        if owner_present {
            let Ok(st) = statat(&wrapper_fd, c"owner.json", AtFlags::SYMLINK_NOFOLLOW) else {
                return;
            };
            if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
                || unlinkat(&wrapper_fd, c"owner.json", AtFlags::empty()).is_err()
            {
                return;
            }
        }
        drop(wrapper_fd);
        let _ = unlinkat(&oms, id, AtFlags::REMOVEDIR);
    }

    // Never inherit the caller's CARGO_TARGET_DIR into a temporary tree: older
    // source mtimes let later real-checkout builds reuse temp-tree binaries.
    pub(crate) fn cargo_env(
        &self,
        provider: &str,
        env: &mut Vec<(OsString, OsString)>,
    ) -> CargoTargetGuard {
        let caller_has_target = std::env::var_os("CARGO_TARGET_DIR").is_some_and(|v| !v.is_empty());
        let cargo_repo = is_cargo_repo(&self.owner) || is_cargo_repo(&self.path);
        let opted_out = cargo_slot_opted_out();
        let slot = acquire_cargo_slot(&self.owner, &self.path, provider);
        env.retain(|(k, _)| k != "CARGO_TARGET_DIR");
        if let Some(slot) = &slot {
            env.push((
                "CARGO_TARGET_DIR".into(),
                slot.target_dir.as_os_str().to_owned(),
            ));
        } else if caller_has_target || (cargo_repo && !opted_out) {
            env.push((
                "CARGO_TARGET_DIR".into(),
                self.path.join("target").into_os_string(),
            ));
        }
        CargoTargetGuard { _slot: slot }
    }
}

pub(crate) fn transition(
    repo: &Path,
    attempt: &str,
    state: &str,
    refs: V,
    context: &Context<'_>,
) -> Result<()> {
    let event = oms_command_operations::new_event(
        attempt,
        0usize.into(),
        "attempt.state_changed",
        obj([
            ("from_state", V::Null),
            ("to_state", state.into()),
            (
                "actor",
                obj([("kind", "harness".into()), ("name", "peer".into())]),
            ),
            ("refs", refs),
        ]),
    )?;
    oms_command_operations::append_lifecycle(repo, event, context.interrupted)?;
    Ok(())
}
#[allow(dead_code)]
pub(crate) fn verify(
    command: &str,
    cwd: &Path,
    context: &Context<'_>,
) -> Result<process::Completed> {
    verify_env(command, cwd, &[], context)
}

pub(crate) fn verify_env(
    command: &str,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    context: &Context<'_>,
) -> Result<process::Completed> {
    process::invoke_env(
        context,
        cwd,
        vec![
            "bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            command.into(),
        ],
        None,
        environment.to_vec(),
        1800,
        true,
    )
}
pub(crate) fn verification(args: &Args, repo: &Path, read: bool) -> Result<Option<String>> {
    if args.has("verify") && args.has("no-verify") {
        return fail("--verify and --no-verify are mutually exclusive");
    }
    if args.has("verify") {
        return Ok(Some(args.required("verify")?.into()));
    }
    if read || args.has("no-verify") {
        return Ok(None);
    }
    let bytes = storage::read(repo, Path::new("scripts/check.sh"), true)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let body = String::from_utf8_lossy(&bytes);
    if body.lines().any(|l| {
        l.trim_start().starts_with("fast)") || l.contains(" fast ") && l.contains("Usage:")
    }) {
        Ok(Some("bash scripts/check.sh fast".into()))
    } else {
        fail("check.sh has no declared fast mode; give --verify CMD or --no-verify")
    }
}
fn graph_context_projection(code: i32, bytes: &[u8]) -> String {
    let unavailable = |reason: &str| {
        format!(
            "{{\"schema\":1,\"kind\":\"oms-delegate-graph-context\",\"delivery_status\":\"error\",\"retrieval\":{{\"status\":\"error\",\"reason\":\"{reason}\"}},\"worker_snapshot_bound\":false,\"text\":\"Graph orientation unavailable; search source directly. Missing matches do not prove missing code.\"}}"
        )
    };
    if code != 0 {
        return unavailable("graph-command-failed");
    }
    if bytes.len() > 128 * 1024 {
        return unavailable("graph-result-exceeds-limit");
    }
    let Ok(mut result) = V::parse(bytes) else {
        return unavailable("malformed-graph-result");
    };
    let status = result.get("retrieval").get("status").text().unwrap_or("");
    if result.get("schema") != &V::Num("1".into())
        || !["matched", "no-match", "partial"].contains(&status)
        || result.get("text").text().is_none()
        || result
            .get("project_graph_revision")
            .text()
            .is_none_or(|s| s.is_empty())
        || !matches!(result.get("truncated"), V::Arr(_))
        || !matches!(result.get("omitted"), V::Arr(_))
    {
        return unavailable("malformed-graph-result");
    }
    let delivery = if status == "matched"
        && (!result.get("truncated").array().is_empty()
            || !result.get("omitted").array().is_empty()
            || result.get("files").array().iter().any(|file| {
                matches!(file.get("incomplete_spans"), V::Num(n) if n.parse::<u64>().is_ok_and(|n| n > 0))
            }))
    {
        "partial"
    } else {
        status
    };
    let delivery = delivery.to_owned();
    result.set("kind", "oms-delegate-graph-context");
    result.set("delivery_status", delivery);
    result.set(
        "source_scope",
        "parent working-tree orientation; not verification or a bound worker snapshot",
    );
    result.set("worker_snapshot_bound", false);
    match result.json(false) {
        Ok(encoded) if encoded.len() <= 16 * 1024 => {
            String::from_utf8(encoded).unwrap_or_else(|_| unavailable("malformed-graph-result"))
        }
        _ => unavailable("graph-projection-exceeds-limit"),
    }
}

fn build_graph_context(
    repo: &Path,
    question: &str,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    crate::check(interrupted)?;
    let is_valid_helper = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            meta.file_type().is_file()
                && (meta.permissions().mode() & 0o111 != 0)
                && (meta.permissions().mode() & 0o022 == 0)
        })
    };
    let helper = std::env::current_exe().ok().and_then(|exe| {
        let sibling = exe
            .parent()
            .map(|dir| dir.join("oms-graph-python-helper"))
            .filter(|p| is_valid_helper(p));
        sibling.or_else(|| {
            exe.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("oms-graph-python-helper"))
                .filter(|p| is_valid_helper(p))
        })
    });
    let argv: Vec<OsString> = vec![
        "project".into(),
        "map".into(),
        "--signatures".into(),
        "--json".into(),
        "--task".into(),
        question.into(),
        "--max-bytes".into(),
        "10000".into(),
        "--repo".into(),
        repo.as_os_str().to_owned(),
    ];
    let (code, stdout, _) =
        oms_command_graph::run_with_helper(&argv, repo, interrupted, helper.as_deref());
    crate::check(interrupted)?;
    Ok(graph_context_projection(code, &stdout))
}

static INTERACTIVE_BUFFERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// In-process callers that consume the returned stdout can opt out of live
/// per-turn NDJSON streaming; the CLI default writes each turn as it completes.
pub fn buffer_interactive_stdout(buffered: bool) {
    INTERACTIVE_BUFFERED.store(buffered, std::sync::atomic::Ordering::SeqCst);
}

fn emit_turn_line(sink: &mut String, line: &[u8]) {
    if INTERACTIVE_BUFFERED.load(std::sync::atomic::Ordering::SeqCst) {
        sink.push_str(&String::from_utf8_lossy(line));
        sink.push('\n');
    } else {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

/// Reads the next turn from stdin; `None` on EOF or an explicit finish, without
/// waiting for the writer to close the pipe.
fn next_interactive_turn() -> Result<Option<String>> {
    use std::io::BufRead;
    let mut stdin = std::io::stdin().lock();
    loop {
        let mut raw = String::new();
        let n = stdin
            .read_line(&mut raw)
            .map_err(|e| Error::new(2, format!("failed reading interactive turns: {e}")))?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('{')
            && let Ok(v) = V::parse(trimmed.as_bytes())
        {
            if v.get("action").text() == Some("finish") || v.get("finish") == &V::Bool(true) {
                return Ok(None);
            }
            if let Some(prompt) = v.get("prompt").text() {
                if prompt.trim().is_empty() {
                    continue;
                }
                return Ok(Some(prompt.trim().to_owned()));
            }
        }
        return Ok(Some(trimmed.to_owned()));
    }
}

fn brief_label(question: &str) -> String {
    for raw in question.lines() {
        let trimmed = raw
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_start_matches("Goal:")
            .trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("goal:") {
            let clean: String = trimmed
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect();
            if !clean.is_empty() {
                return clean;
            }
        }
    }
    "peer-delegate".into()
}

fn bound_task_id(args: &Args) -> Option<String> {
    for key in ["plan-task", "task-id"] {
        let value = args.get(key).trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn active_task_id(packet: &[u8]) -> Option<&[u8]> {
    if oms_command_task::packet::metadata(packet, "status").map(oms_command_task::packet::trim)
        == Some(b"closed")
    {
        return None;
    }
    oms_command_task::packet::metadata(packet, "task_id")
        .map(oms_command_task::packet::trim)
        .filter(|id| !id.is_empty())
}

fn should_attach_task_packet(args: &Args, read: bool, packet: &[u8]) -> bool {
    if args.has("no-task") || packet.is_empty() {
        return false;
    }
    if args.has("task") {
        return true;
    }
    if read {
        return false;
    }
    let Some(bound) = bound_task_id(args) else {
        return false;
    };
    active_task_id(packet).is_some_and(|active| active == bound.as_bytes())
}

pub(crate) fn run(args: &Args, context: &Context<'_>) -> Result<Output> {
    safety::parent()?;
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let mut resolved = args.clone();
    let mut plan = crate::plan_delegate::BoundDelegate::prepare(&mut resolved, &repo, context)?;
    let args = &resolved;
    let interactive = args.has("interactive");
    let orientation = if args.has("context-pack") {
        Some(crate::context_pack::orientation(
            &args.path("context-pack", context.cwd),
        )?)
    } else {
        None
    };
    let mut target = Target::parse(args.required("to")?)?;
    peer::apply_account(args, std::slice::from_mut(&mut target))?;
    let question = peer::question(args, context)?;
    let directory = peer::directory(args, &repo, "delegate")?;
    let mut read = args.has("read-only");
    if args.has("role")
        && !read
        && std::env::var_os("OMS_DELEGATE_ROLE_WRITE").is_none_or(|v| v != "1")
    {
        let policy = providers::config(context, "role-contracts.json")?;
        read = policy
            .get("roles")
            .get(args.get("role"))
            .get("read_only")
            .truth();
    }
    let command = verification(args, &repo, read)?;
    let repair = if args.has("repair") {
        args.limit("repair", 0, 1, 3)?
    } else {
        0
    };
    if !["standard", "routine"].contains(&args.value("workload", "standard")) {
        return fail("--workload requires standard or routine");
    }
    let scope = args
        .all("scope")
        .into_iter()
        .map(|s| oms_command_room::normalize_scope(&s).map_err(|e| Error::new(2, e.to_string())))
        .collect::<Result<Vec<_>>>()?;
    for pattern in &scope {
        oms_contract_markdown::scope::validate_pattern(pattern)
            .map_err(|e| Error::new(2, e.to_string()))?;
    }
    let brief_issues = lint(&question);
    if args.has("strict-brief") && !brief_issues.is_empty() {
        return fail(format!("brief lacks {}", brief_issues.join(", ")));
    }
    let role = if read { "researcher" } else { "worker" };
    let mut route = providers::route(
        context,
        &target,
        args.get("model"),
        args.value("reasoning-effort", "auto"),
        role,
    )?;
    if let Some(plan) = &plan {
        if read {
            return fail("Plan patch delegation cannot change into a read-only role");
        }
        plan.route(args, &route)?;
    }
    route.explicit = !providers::panel_implicit_target() || route.account_pinned;
    route.public_research = read
        && matches!(args.get("role"), "researcher" | "scholar")
        && std::env::var_os("OMS_PANEL_NO_WEB").is_none_or(|value| value != "1");
    providers::bind_account(&mut route, &repo)?;
    let run = peer::run_id()?;
    let artifact = directory.join(format!("{}-delegate-{run}.md", target.provider));
    let patch = directory.join(format!("{}-delegate-{run}.patch", target.provider));
    let base = String::from_utf8_lossy(&process::git(context, &repo, &["rev-parse", "HEAD"])?)
        .trim()
        .to_owned();
    let mut prompt = crate::request_context::Prompt::framing(
        "You are a delegated worker in an isolated repository worktree. Follow repository contracts. Do not run git commit or git push. Do not recursively delegate. The owner retains review, admission and publication.\nGoal:\n",
    );
    prompt.push_layer("task", &question);
    prompt.push_layer("framing", &format!("\n\nOutput: return patch artifacts and concise changed files, checks, uncertainty and blockers.\nDone when: the authorized task and its required verification are complete, or no work can advance without the owner.\nBase revision: {base}\n"));
    if !args.has("no-task") && (args.has("task") || (!read && bound_task_id(args).is_some())) {
        let packet = storage::read(&repo, Path::new(".oms/task/current.md"), true)?;
        if should_attach_task_packet(args, read, &packet) {
            prompt.untrusted("task", "task", "recorded context", &packet)?;
        }
    }
    if args.has("memory") && !args.has("no-memory") {
        let (code, bytes, _) =
            oms_command_memory::provider_context(&repo, &question, context.interrupted);
        if code != 0 {
            return fail("shared memory context failed");
        }
        prompt.untrusted("retrieved", "memory", "recorded context", &bytes)?;
    }
    if args.has("ml-context") && !args.has("no-ml-context") {
        let digest = crate::ml_context::build(&repo, context)?;
        if !digest.is_empty() {
            prompt.untrusted(
                "retrieved",
                "ML project",
                "context digest",
                digest.as_bytes(),
            )?;
        }
    }
    if let Some(orientation) = &orientation {
        prompt.untrusted(
            "retrieved",
            "parent graph",
            "orientation metadata",
            orientation.orientation.as_bytes(),
        )?;
    } else if args.has("graph-context")
        && !args.has("no-graph-context")
        && !args.has("context-pack")
    {
        let graph_text = build_graph_context(&repo, &question, context.interrupted)?;
        prompt.untrusted(
            "retrieved",
            "project graph",
            "retrieval status and source orientation",
            graph_text.as_bytes(),
        )?;
    }
    if read {
        prompt.push_str(
            "Read-only investigation: do not modify files. Return evidence and findings.\n",
        );
    }
    if !scope.is_empty() {
        prompt.push_str(&format!("Allowed paths: {}\n", scope.join(", ")));
    }
    if args.has("role") {
        let (code, bytes, _) = oms_command_agent_role::run_with_source(
            &[
                "show".into(),
                "--name".into(),
                args.get("role").into(),
                "--repo".into(),
                repo.as_os_str().into(),
            ],
            &repo,
            context.source,
            context.interrupted,
        );
        if code != 0 {
            return fail("role profile could not be resolved");
        }
        prompt.push_layer("contract", &String::from_utf8_lossy(&bytes));
    }
    if args.has("review-artifact") {
        prompt.untrusted(
            "history",
            "reviewer",
            "review claims",
            &storage::file(&args.path("review-artifact", context.cwd))?,
        )?;
    }
    safety::outbound(prompt.as_bytes())?;
    let mut header = format!(
        "# OMS peer-delegate\n\n- provider: {}\n- base_sha: {base}\n- access: {}\n\n## Prompt\n\n{prompt}\n",
        target.provider,
        if read { "read" } else { "write" }
    );
    storage::write(
        &repo,
        &artifact,
        header.as_bytes().to_vec(),
        false,
        context.interrupted,
    )?;
    storage::write(&repo, &patch, Vec::new(), false, context.interrupted)?;
    if peer::dry(args, "peer-delegate") || args.has("export-only") {
        let advice = obj([
            ("advisory", true.into()),
            (
                "verdict",
                if read {
                    "researcher"
                } else if question.len() < 600 {
                    "inline"
                } else if !scope.is_empty() && command.is_some() {
                    "worker"
                } else {
                    "researcher"
                }
                .into(),
            ),
            (
                "facts",
                obj([
                    ("brief_bytes", V::Num(question.len().to_string())),
                    ("has_check", command.is_some().into()),
                ]),
            ),
        ]);
        let mut output = format!(
            "{}\n{}\n",
            repo.join(&artifact).display(),
            repo.join(&patch).display()
        );
        if args.has("json") {
            output.push_str(&format!(
                "advice-json: {}\n",
                String::from_utf8_lossy(&advice.json(false)?)
            ));
        }
        return Ok(out(output.into_bytes()));
    }
    if interactive && std::io::stdin().is_terminal() {
        return fail("--interactive requires NDJSON turn input on stdin; no worktree created");
    }
    let worktree = Worktree::create(&repo, &base, context)?;
    if let Some(bound) = &mut plan {
        let generation = worktree.bound_start_generation()?;
        if let Err(error) = bound.start(&worktree, generation, context) {
            // Preserve a checkout whose original contents cannot be proved.
            if worktree
                .bound_start_generation()
                .is_ok_and(|seen| seen == generation)
                && worktree.pristine(context).is_ok()
                && worktree
                    .bound_start_generation()
                    .is_ok_and(|seen| seen == generation)
            {
                let _ = worktree.remove(context);
            }
            return Err(error);
        }
    }
    let compiled = if args.has("context-manifest") {
        let prepare = || -> Result<_> {
            worktree.pristine(context)?;
            let compiled = crate::compiled_context::Compiled::build(
                &repo,
                &worktree.path,
                orientation
                    .as_ref()
                    .map_or(&[], |pack| pack.targets.as_slice()),
                context,
            )?;
            worktree.pristine(context)?;
            Ok(compiled)
        };
        let prepared = prepare().and_then(|compiled| {
            let prior = prompt.len();
            prompt.compiled_context(&compiled)?;
            safety::outbound(prompt.as_bytes())?;
            header.push_str(&prompt[prior..]);
            Ok(compiled)
        });
        match prepared {
            Ok(compiled) => Some(compiled),
            Err(error) => {
                // A status observation does not reserve the snapshot against concurrent writes.
                let retained = format!("context snapshot retained: {}", worktree.path.display());
                storage::write(&repo, &artifact,
                    format!("{header}\n## Context preparation\n\nCompilation or outbound validation failed; no worker started.\n{retained}\n").into_bytes(),
                    true, context.interrupted)
                    .map_err(|e| Error::new(e.code, format!("{}; {retained}", e.message)))?;
                return Err(Error::new(
                    error.code,
                    format!("{}; {retained}", error.message),
                ));
            }
        }
    } else {
        None
    };
    let mut attempt_refs = obj([
        ("artifact", artifact.to_string_lossy().into_owned().into()),
        ("patch", patch.to_string_lossy().into_owned().into()),
        (
            "worktree",
            worktree
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        ),
    ]);
    let dispatch = crate::dispatch_context::DispatchContext::from_environment(args.get("task-id"));
    let task_id = dispatch.task_id.clone();
    dispatch.delegate_refs(
        &mut attempt_refs,
        &route,
        &target.provider,
        read,
        &brief_label(&question),
    );
    let attempt = oms_command_operations::create_attempt(
        &repo,
        &oms_command_operations::StartRequest {
            provider: target.provider.clone(),
            tool: "peer-delegate".into(),
            task_id: task_id.clone(),
            parent_attempt_id: dispatch.parent_attempt_id.clone().unwrap_or_default(),
            refs: attempt_refs,
            ..Default::default()
        },
        context.interrupted,
    )?;
    transition(&repo, &attempt, "starting", obj([]), context)?;
    transition(&repo, &attempt, "working", obj([]), context)?;
    let mut code = 1;
    let mut body = header.into_bytes();
    let mut captured = Vec::new();
    let mut written_patch = Vec::new();
    let mut verified = "not_run";
    let mut verify_exit_code: Option<i32> = None;
    let mut needs_keep = args.has("keep-worktree");
    let mut interactive_stdout = String::new();
    let mut extra_env = Vec::new();
    let cargo_guard = worktree.cargo_env(&target.provider, &mut extra_env);
    extra_env = crate::usage::operation_env(&extra_env)?;
    if let Some(bound) = &plan {
        extra_env.extend(bound.launch_environment()?);
    }
    process::TeamLocator::append_args(args, &mut extra_env)?;
    extra_env.push(("OMS_ATTEMPT_ID".into(), attempt.clone().into()));
    if !task_id.is_empty() {
        extra_env.push(("OMS_TASK_ID".into(), task_id.clone().into()));
    }
    for round in 0..=repair {
        if round > 0 && !route.account_pinned {
            route.account.clear();
        }
        crate::check(context.interrupted)?;
        verify_exit_code = None;
        let mut bound_verifier_failed = false;
        worktree.identity(context)?;
        if interactive && round == 0 {
            let mut turn_history = String::new();
            let timeout = peer::timeout(args, "peer-delegate")?;
            code = 0;
            let max_turns = args
                .get("max-turns")
                .parse::<usize>()
                .ok()
                .map_or(20, |n| n.clamp(1, 20));
            let mut idx = 0;
            while idx < max_turns {
                let turn_prompt = match next_interactive_turn()? {
                    Some(turn) => turn,
                    None if idx == 0 => String::new(),
                    None => break,
                };
                crate::check(context.interrupted)?;
                worktree.identity(context)?;
                let mut full_prompt = prompt.clone();
                if !turn_history.is_empty() {
                    full_prompt.push_str("\n## Prior Interactive Turns\n");
                    full_prompt.push_layer("history", &turn_history);
                }
                if !turn_prompt.is_empty() {
                    full_prompt.push_layer("framing", &format!("\n## Turn {} Input\n", idx + 1));
                    full_prompt.push_layer("task", &turn_prompt);
                    full_prompt.push_layer("framing", "\n");
                }
                safety::outbound(full_prompt.as_bytes())?;
                let res = providers::execute_with_fallback_env(
                    &mut route,
                    args.get("fallback-model"),
                    &full_prompt,
                    &worktree.path,
                    !read,
                    timeout,
                    &repo,
                    &extra_env,
                    context,
                );
                let res = match res {
                    Ok(r) => r,
                    Err(error) => {
                        code = error.code;
                        body.extend_from_slice(
                            format!("\n## Turn {} Error\n\n{}\n", idx + 1, error.message)
                                .as_bytes(),
                        );
                        let line = obj([
                            ("turn", V::Num((idx + 1).to_string())),
                            ("code", V::Num(code.to_string())),
                            ("complete", false.into()),
                            ("error", error.message.into()),
                        ])
                        .json(false)?;
                        emit_turn_line(&mut interactive_stdout, &line);
                        break;
                    }
                };
                let turn_code = if !res.complete && res.code == 0 {
                    1
                } else {
                    res.code
                };
                let clean = safety::redact(&res.stdout)?;
                let clean_str = String::from_utf8_lossy(&clean).into_owned();
                body.extend_from_slice(
                    format!("\n## Turn {}\n\n{clean_str}\n", idx + 1).as_bytes(),
                );
                turn_history.push_str(&format!(
                    "\nTurn {}: {}\nReply: {}\n",
                    idx + 1,
                    turn_prompt,
                    clean_str.chars().take(4000).collect::<String>()
                ));
                if turn_history.len() > 24 * 1024 {
                    let drop_bytes = turn_history.len() - 24 * 1024;
                    let boundary = turn_history
                        .char_indices()
                        .find(|(pos, _)| *pos >= drop_bytes)
                        .map_or(turn_history.len(), |(pos, _)| pos);
                    turn_history.drain(..boundary);
                }
                let line = obj([
                    ("turn", V::Num((idx + 1).to_string())),
                    ("code", V::Num(turn_code.to_string())),
                    ("complete", res.complete.into()),
                    ("output", clean_str.into()),
                ])
                .json(false)?;
                emit_turn_line(&mut interactive_stdout, &line);
                code = turn_code;
                if turn_code != 0 {
                    break;
                }
                idx += 1;
            }
        } else {
            let result = plan
                .as_ref()
                .map_or(Ok(()), |bound| bound.check(context))
                .and_then(|()| {
                    providers::execute_with_fallback_env(
                        &mut route,
                        args.get("fallback-model"),
                        &prompt,
                        &worktree.path,
                        !read,
                        peer::timeout(args, "peer-delegate")?,
                        &repo,
                        &extra_env,
                        context,
                    )
                });
            let result = match result {
                Ok(r) => r,
                Err(error) => {
                    code = error.code;
                    needs_keep |= plan.is_some();
                    body.extend_from_slice(
                        format!("\n## Output\n\n{}\n", error.message).as_bytes(),
                    );
                    break;
                }
            };
            code = result.code;
            if !result.complete {
                if code == 0 {
                    code = 1;
                }
                if plan.is_some() {
                    needs_keep = true;
                    body.extend_from_slice(
                        b"\nPlan worker delivery is unknown; worktree retained for reconciliation.\n",
                    );
                }
            }
            body.extend_from_slice(
                format!(
                    "\n## Output\n\n{}\n",
                    String::from_utf8_lossy(&safety::redact(&result.stdout)?)
                )
                .as_bytes(),
            );
        }
        worktree.identity(context)?;
        process::git(
            context,
            &worktree.path,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "add",
                "-A",
                "--",
            ],
        )?;
        let names = process::git(
            context,
            &worktree.path,
            &["diff", "--cached", "--name-only", "-z", &base, "--"],
        )?;
        for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let name = std::str::from_utf8(name)
                .map_err(|_| Error::new(2, "changed path is not UTF-8"))?;
            if name.starts_with(".git/")
                || name == ".git"
                || name.starts_with(".oms/")
                || name == ".oms"
                || plan
                    .as_ref()
                    .is_some_and(|bound| bound.forbidden(name).unwrap_or(true))
                || !scope.is_empty()
                    && !scope
                        .iter()
                        .any(|s| oms_contract_markdown::scope::matches(name, s).unwrap_or(false))
            {
                needs_keep = true;
                code = 1;
                body.extend_from_slice(
                    b"\nWorker changed a protected or out-of-scope path; patch withheld.\n",
                );
                break;
            }
        }
        if code == 0 || !names.is_empty() {
            captured = worktree.staged_patch(context)?;
        }
        if read && !captured.is_empty() {
            code = 1;
            needs_keep = true;
            body.extend_from_slice(b"\nRead-only worker changed files; worktree retained.\n");
        }
        written_patch = if needs_keep && code != 0 {
            Vec::new()
        } else {
            captured.clone()
        };
        storage::write(
            &repo,
            &patch,
            written_patch.clone(),
            true,
            context.interrupted,
        )?;
        if code == 0
            && !captured.is_empty()
            && let Some(command) = &command
        {
            transition(&repo, &attempt, "verifying", obj([]), context)?;
            let checked = verify_env(command, &worktree.path, &extra_env, context)?;
            code = checked.code;
            if !checked.complete && code == 0 {
                code = 1;
            }
            let boundary = worktree.identity(context).is_ok()
                && process::git(
                    context,
                    &worktree.path,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--binary",
                        "--src-prefix=a/",
                        "--dst-prefix=b/",
                        &base,
                        "--",
                    ],
                )
                .is_ok_and(|after| after == captured);
            if !boundary {
                code = 125;
                needs_keep = true;
                body.extend_from_slice(b"\nVerifier changed the pinned tracked state; worktree retained and success withheld.\n");
            }
            verify_exit_code = Some(code);
            bound_verifier_failed = checked.complete && !matches!(code, 0 | 124);
            verified = if code == 0 { "passed" } else { "failed" };
            let clean = safety::redact(&checked.stdout)?;
            body.extend_from_slice(
                format!(
                    "\n## Verification\n\n{}\n\nexit: {code}\n",
                    String::from_utf8_lossy(&clean)
                )
                .as_bytes(),
            );
            if code != 0 {
                prompt.push_str(&safety::untrusted(
                    "verifier",
                    "verification output",
                    &clean,
                )?);
            }
            if code != 0 && round < repair {
                transition(&repo, &attempt, "working", obj([]), context)?;
            }
        }
        storage::write(&repo, &artifact, body.clone(), true, context.interrupted)?;
        if code == 0
            || matches!(code, 3 | 126 | 127 | 130)
            || needs_keep
            || (plan.is_some() && !bound_verifier_failed)
        {
            break;
        }
        if plan.is_some() && round < repair {
            worktree.prepare_bound_repair(&captured, context)?;
        }
        prompt.push_str(
            "\nRepair the rejected changes and verification failure within the original scope.\n",
        );
    }
    drop(cargo_guard);
    let no_change = !read && captured.is_empty();
    if no_change && !args.has("allow-no-change") && dispatch.panel.is_some() {
        code = 6;
    }
    let state = if code == 0 {
        "review"
    } else if code == 124 {
        "timed_out"
    } else if code == 130 {
        "cancelled"
    } else {
        "failed"
    };
    let mut refs = obj([
        ("artifact", artifact.to_string_lossy().into_owned().into()),
        ("patch", patch.to_string_lossy().into_owned().into()),
        ("verify_status", verified.into()),
        ("admit", "pending".into()),
    ]);
    if !providers::panel_account(&route).is_empty() {
        // An overflow may have moved the run off the account bound at start.
        refs.set("panel_account", V::from(providers::panel_account(&route)));
    }
    transition(&repo, &attempt, state, refs, context)?;
    body.extend_from_slice(format!("\n## Exit\n\n{code}\n\n- outcome: {}\n- verification: {verified}\n- admission: pending\n",if no_change{"no_change"}else if code==0{"review"}else{"failed"}).as_bytes());
    let artifact_sha256 = storage::hash(&body);
    let patch_sha256 = storage::hash(&written_patch);
    storage::write(&repo, &artifact, body, true, context.interrupted)?;
    let mut index = obj([
        ("schema", 1usize.into()),
        ("event_id", crate::id("evt_")?.into()),
        ("kind", "delegate".into()),
        ("ts", now().into()),
        ("artifact_id", crate::id("art_")?.into()),
        ("attempt_id", attempt.clone().into()),
        (
            "operation_id",
            extra_env
                .iter()
                .rev()
                .find(|(k, _)| k == "OMS_OPERATION_ID")
                .unwrap()
                .1
                .to_string_lossy()
                .into_owned()
                .into(),
        ),
        ("task_id", task_id.into()),
        ("provider", target.provider.into()),
        ("model", route.model.into()),
        ("artifact", artifact.to_string_lossy().into_owned().into()),
        ("artifact_sha256", artifact_sha256.into()),
        ("patch", patch.to_string_lossy().into_owned().into()),
        ("patch_sha256", patch_sha256.into()),
        ("base_sha", base.into()),
        ("exit", V::Num(code.to_string())),
        ("verification", verified.into()),
        (
            "verify_exit",
            verify_exit_code.map_or(V::Null, |n| V::Num(n.to_string())),
        ),
        ("state", state.into()),
        ("admit", "pending".into()),
        ("no_change", no_change.into()),
    ]);
    if let Some(compiled) = &compiled {
        compiled.index(&mut index);
    }
    if let Some(plan) = &plan {
        index.set("plan_binding", plan.issued()?);
    }
    peer::index(&repo, &index, context)?;
    let plan_review = if code == 0 {
        plan.as_ref().map_or(Ok(()), |bound| {
            bound.review(
                &index,
                extra_env
                    .iter()
                    .rev()
                    .find(|(key, _)| key == "OMS_OPERATION_ID")
                    .and_then(|(_, value)| value.to_str())
                    .unwrap_or(""),
                &attempt,
                context,
            )
        })
    } else {
        Ok(())
    };
    let mut note = String::new();
    if needs_keep {
        note.push_str(&format!("worktree retained: {}\n", worktree.path.display()));
    } else if let Err(e) = worktree.remove(context) {
        note.push_str(&format!("worktree cleanup deferred: {}\n", e.message));
    }
    plan_review?;
    let next = if code == 0 && !written_patch.is_empty() {
        let repo_arg = shell_arg(&repo);
        let patch_arg = shell_arg(&patch);
        format!(
            "next: oms patch-land --repo {repo_arg} --patch {patch_arg} (or oms patch-admit --repo {repo_arg} --patch {patch_arg})\n"
        )
    } else {
        String::new()
    };
    Ok((
        code,
        format!(
            "{interactive_stdout}{}\n{}\n{next}",
            repo.join(artifact).display(),
            repo.join(patch).display()
        )
        .into_bytes(),
        note.into_bytes(),
    ))
}

fn shell_arg(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if !raw.is_empty()
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+:@".contains(&b))
    {
        raw.into_owned()
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}
fn lint(body: &str) -> Vec<&'static str> {
    if body.len() < 240 {
        return Vec::new();
    }
    let lower = body.to_lowercase();
    let mut issues = Vec::new();
    for (label, words) in [
        ("goal", &["goal:", "objective:", "purpose:"][..]),
        ("scope", &["scope:", "constraints:", "out of scope:"][..]),
        ("output", &["output:", "return ", "deliverable:"][..]),
        (
            "stop condition",
            &["done when:", "stop if:", "success criteria:"][..],
        ),
    ] {
        if !words.iter().any(|w| lower.contains(w)) {
            issues.push(label);
        }
    }
    if !body.contains('/') && !cached_regex!(r"\b[\w-]+\.(rs|py|sh|md|json|toml)\b").is_match(body)
    {
        issues.push("file pointers");
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oms-peer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        dir
    }

    #[test]
    fn graph_context_keeps_no_match_coverage_and_does_not_change_discovery() {
        let repo = repo("graph-context");
        std::fs::write(
            repo.join("lib.rs"),
            "pub fn graph_context_probe() -> usize { 42 }\n",
        )
        .unwrap();
        git(&repo, &["add", "lib.rs"]);
        git(&repo, &["commit", "-qm", "graph fixture"]);
        let stop = AtomicBool::new(false);
        let matched = build_graph_context(&repo, "graph_context_probe", &stop).unwrap();
        let parsed = V::parse(matched.as_bytes()).unwrap();
        assert_eq!(
            parsed.get("delivery_status").text(),
            Some("matched"),
            "{matched}"
        );
        assert!(
            parsed
                .get("text")
                .text()
                .unwrap()
                .contains("fn graph_context_probe")
        );
        assert_eq!(parsed.get("worker_snapshot_bound"), &V::Bool(false));
        let manifest = repo.join(".oms/project-graph/manifest.json");
        let before = std::fs::read(&manifest).unwrap();
        let missing = build_graph_context(&repo, "unmatchedzzzzfixture", &stop).unwrap();
        let parsed = V::parse(missing.as_bytes()).unwrap();
        assert_eq!(
            parsed.get("retrieval").get("status").text(),
            Some("no-match")
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        let partial = graph_context_projection(0, br#"{"schema":1,"project_graph_revision":"fixture","retrieval":{"status":"matched"},"text":"fn wanted()","truncated":["lib.rs"],"omitted":["more.rs"]}"#);
        let parsed = V::parse(partial.as_bytes()).unwrap();
        assert_eq!(parsed.get("delivery_status").text(), Some("partial"));
        assert_eq!(parsed.get("omitted").array()[0].text(), Some("more.rs"));
        let incomplete = graph_context_projection(0, br#"{"schema":1,"project_graph_revision":"fixture","retrieval":{"status":"matched"},"text":"fn wanted()","files":[{"path":"lib.rs","incomplete_spans":1}],"truncated":[],"omitted":[]}"#);
        assert_eq!(
            V::parse(incomplete.as_bytes())
                .unwrap()
                .get("delivery_status")
                .text(),
            Some("partial")
        );
        for (code, bytes) in [
            (1, b"private diagnostic".as_slice()),
            (0, b"not-json".as_slice()),
        ] {
            let projection = graph_context_projection(code, bytes);
            assert_eq!(
                V::parse(projection.as_bytes())
                    .unwrap()
                    .get("retrieval")
                    .get("status")
                    .text(),
                Some("error")
            );
            assert!(!projection.contains("private diagnostic"));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(build_graph_context(&repo, "probe", &stop).is_err());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn remove_reclaims_wrapper_only_after_clean_removal() {
        let repo = repo("wrapper");
        let stop = AtomicBool::new(false);
        let native = process::Native;
        let context = Context {
            cwd: &repo,
            source: &repo,
            interrupted: &stop,
            executor: &native,
        };
        let tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        let wrapper = tree.path.parent().unwrap().to_owned();
        assert!(wrapper.join("owner.json").is_file());
        // A changed HEAD preserves everything, wrapper included.
        git(
            &tree.path,
            &["commit", "-q", "--allow-empty", "-m", "drift"],
        );
        let drifted = Worktree {
            path: tree.path.clone(),
            base: tree.base.clone(),
            owner: tree.owner.clone(),
        };
        assert!(drifted.remove(&context).is_err());
        assert!(tree.path.is_dir() && wrapper.join("owner.json").is_file());
        git(&tree.path, &["reset", "-q", "--hard", &tree.base]);
        tree.remove(&context).unwrap();
        assert!(!wrapper.exists(), "wrapper dir reclaimed");
        assert!(repo.join(".oms/tmp").is_dir());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn remove_leaves_wrapper_with_unexpected_entries_untouched() {
        struct LockProbingExecutor {
            repo: PathBuf,
            saw_busy_during_worktree_add: AtomicBool,
        }
        impl process::Executor for LockProbingExecutor {
            fn execute(
                &self,
                request: process::Invocation,
                interrupted: &AtomicBool,
            ) -> Result<process::Completed> {
                let words: Vec<String> = request
                    .argv
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if words.windows(2).any(|w| w == ["worktree", "add"])
                    && let Some(tree_arg) = words.iter().find(|w| w.ends_with("/tree"))
                {
                    let owner = Path::new(tree_arg).parent().unwrap().join("owner.json");
                    assert!(owner.is_file(), "owner.json created under held lock");
                    let mut settings = oms_lock::Settings::from_env().unwrap();
                    settings.cwd = self.repo.clone();
                    let lock = oms_lock::Lock::for_file(&owner, settings).unwrap();
                    assert!(
                        matches!(lock.try_acquire(), Err(oms_lock::Error::Busy)),
                        "owner.json lock must remain held across git worktree add"
                    );
                    self.saw_busy_during_worktree_add
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                process::Native.execute(request, interrupted)
            }
        }

        let repo = repo("extra");
        let stop = AtomicBool::new(false);
        let probing = LockProbingExecutor {
            repo: repo.clone(),
            saw_busy_during_worktree_add: AtomicBool::new(false),
        };
        let context = Context {
            cwd: &repo,
            source: &repo,
            interrupted: &stop,
            executor: &probing,
        };
        let tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        assert!(
            probing
                .saw_busy_during_worktree_add
                .load(std::sync::atomic::Ordering::Acquire)
        );
        let wrapper = tree.path.parent().unwrap().to_owned();
        std::os::unix::fs::symlink(&repo, wrapper.join("link")).unwrap();
        tree.remove(&context).unwrap();
        assert!(wrapper.join("owner.json").is_file(), "owner.json kept");
        assert!(wrapper.join("link").exists());

        // Swapping owner.json with a symlink must leave the target untouched.
        let victim = repo.join("victim.txt");
        std::fs::write(&victim, b"preserve").unwrap();
        let sym_tree = Worktree::create(&repo, "HEAD", &context).unwrap();
        let sym_wrapper = sym_tree.path.parent().unwrap().to_owned();
        std::fs::remove_file(sym_wrapper.join("owner.json")).unwrap();
        std::os::unix::fs::symlink(&victim, sym_wrapper.join("owner.json")).unwrap();
        sym_tree.remove(&context).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
        assert!(sym_wrapper.join("owner.json").is_symlink());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn task_packet_attachment_matches_task_bindings_and_flags() {
        let parse = |raw: &[&str]| {
            let argv: Vec<OsString> = raw.iter().map(OsString::from).collect();
            peer::parse(&argv).unwrap()
        };
        let active =
            b"# Active Agent Task\n\n- task_id: task-459\n- status: active\n\n## Goal\nWork\n";
        let closed =
            b"# Active Agent Task\n\n- task_id: task-459\n- status: closed\n\n## Goal\nWork\n";
        let unnamed = b"# Active Agent Task\n\n- status: active\n\n## Goal\nWork\n";

        assert!(!should_attach_task_packet(&parse(&[]), false, active));
        assert!(!should_attach_task_packet(
            &parse(&["--task-id", "other"]),
            false,
            active
        ));
        assert!(should_attach_task_packet(
            &parse(&["--task-id", "task-459"]),
            false,
            active
        ));
        assert!(should_attach_task_packet(
            &parse(&["--plan-task", "task-459"]),
            false,
            active
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--plan-task", "other"]),
            false,
            active
        ));

        // Read delegations stay opt-in via --task even when bound to the same task.
        assert!(!should_attach_task_packet(
            &parse(&["--task-id", "task-459"]),
            true,
            active
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--plan-task", "task-459"]),
            true,
            active
        ));
        assert!(should_attach_task_packet(&parse(&["--task"]), true, active));

        // Explicit --task / --no-task flags take precedence.
        assert!(should_attach_task_packet(
            &parse(&["--task"]),
            false,
            active
        ));
        assert!(should_attach_task_packet(
            &parse(&["--task"]),
            false,
            closed
        ));
        assert!(should_attach_task_packet(
            &parse(&["--task"]),
            false,
            unnamed
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--task", "--no-task"]),
            false,
            active
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--plan-task", "task-459", "--no-task"]),
            false,
            active
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--task-id", "task-459"]),
            false,
            closed
        ));
        assert!(!should_attach_task_packet(
            &parse(&["--task-id", "task-459"]),
            false,
            unnamed
        ));
        assert!(!should_attach_task_packet(&parse(&["--task"]), false, b""));
    }

    #[test]
    fn help_options_match_parser_spec_and_omit_removed_flags() {
        let help = std::str::from_utf8(peer::help("peer-delegate")).unwrap();
        for removed in ["--apply", "--resume-session", "--observe-plan-binding"] {
            assert!(
                !help.contains(removed),
                "removed/refused flag documented in help: {removed}"
            );
        }
        assert!(
            peer::parse(&["--apply".into()])
                .unwrap_err()
                .message
                .contains("unknown option: --apply")
        );
        assert!(
            peer::parse(&["--resume-session".into(), "s1".into()])
                .unwrap_err()
                .message
                .contains("unknown option: --resume-session")
        );
        assert!(
            peer::parse(&["--observe-plan-binding".into(), "{}".into()])
                .unwrap_err()
                .message
                .contains("unknown option: --observe-plan-binding")
        );

        let options_block = help
            .split_once("\nOptions:\n")
            .and_then(|(_, rest)| rest.split_once("\nEnvironment:\n"))
            .map(|(opts, _)| opts)
            .expect("Options section in peer-delegate help");
        let mut parsed_entries = Vec::new();
        for line in options_block.lines() {
            let Some(header) = line.strip_prefix("  ") else {
                continue;
            };
            if !header.starts_with('-') {
                continue;
            }
            let spec = header.split("  ").next().unwrap().trim();
            if spec == "-h, --help" {
                parsed_entries.push(("-h".to_owned(), false));
                parsed_entries.push(("--help".to_owned(), false));
                continue;
            }
            let tokens: Vec<&str> = spec.split_whitespace().collect();
            let mut idx = 0;
            while idx < tokens.len() {
                let tok = tokens[idx];
                if !tok.starts_with("--") {
                    break;
                }
                let takes_value = tokens.get(idx + 1).is_some_and(|next| {
                    !next.starts_with('-') && next.bytes().all(|b| b.is_ascii_uppercase())
                });
                parsed_entries.push((tok.to_owned(), takes_value));
                idx += if takes_value { 2 } else { 1 };
            }
        }
        assert!(
            parsed_entries.len() >= 40,
            "unexpectedly few options parsed: {}",
            parsed_entries.len()
        );
        for (opt, takes_value) in &parsed_entries {
            if *takes_value {
                assert!(
                    peer::parse(&[opt.as_str().into(), "fixture".into()]).is_ok(),
                    "valued option from --help rejected by parser: {opt}"
                );
                assert!(
                    peer::parse(&[opt.as_str().into()])
                        .unwrap_err()
                        .message
                        .contains("requires a value"),
                    "valued option in --help did not require a value: {opt}"
                );
            } else {
                assert!(
                    peer::parse(&[opt.as_str().into()]).is_ok(),
                    "flag from --help rejected by parser: {opt}"
                );
            }
        }

        // Every --flag reference anywhere in help prose/usage must also be known to the parser.
        for mat in regex::Regex::new(r"--[a-z0-9][a-z0-9-]*")
            .unwrap()
            .find_iter(help)
        {
            let name = &mat.as_str()[2..];
            assert!(
                name == "help" || peer::VALUES.contains(&name) || peer::FLAGS.contains(&name),
                "help mentions unknown option --{name}"
            );
        }
    }
}
