//! Exact-HEAD delivery receipts and read-only lock status. Receipt state never
//! substitutes for a fresh lock observation, successful exact-SHA CI or proof
//! that the full selection executed.
use crate::{
    Context, Error, Output, Result, V, admission, ci,
    cli::Options,
    digest,
    git::{Git, Snapshot},
    now, obj, parent, require, storage, string,
    worktree::{self, Sandbox},
};
use std::{
    ffi::OsStr,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
pub(super) mod pr_observation;

fn cksum(raw: &[u8]) -> (u32, usize) {
    let mut crc = 0u32;
    let add = |crc: &mut u32, b: u8| {
        *crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            *crc = if *crc & 0x80000000 != 0 {
                (*crc << 1) ^ 0x04c11db7
            } else {
                *crc << 1
            };
        }
    };
    for b in raw {
        add(&mut crc, *b);
    }
    let mut size = raw.len();
    while size != 0 {
        add(&mut crc, (size & 255) as u8);
        size >>= 8;
    }
    (!crc, raw.len())
}
pub(crate) fn state_directory(git: &Git<'_>) -> Result<PathBuf> {
    let common = worktree::common(git)?;
    let root = std::fs::canonicalize(
        common
            .parent()
            .ok_or_else(|| Error::new(2, "common directory has no parent"))?,
    )?;
    let name = root.file_name().unwrap_or_default().to_string_lossy();
    let name = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .replace("..", "_");
    let name = if matches!(name.as_str(), "" | "." | "..") {
        String::from("repo")
    } else {
        name
    };
    let (crc, size) = cksum(root.as_os_str().as_encoded_bytes());
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .ok_or_else(|| Error::new(2, "land state home unavailable"))?;
    Ok(state
        .join("oh-my-setting/land")
        .join(format!("{name}-{crc}-{size}")))
}
pub(crate) fn request_stamp(head: &str, parts: &[&str]) -> Result<String> {
    let request = V::Arr(parts.iter().map(|s| V::from(*s)).collect());
    let value = oms_command_plan::json::parse(&request.json(false)?)?;
    let hash = digest(&oms_command_plan::json::default_bytes(&value, true)?);
    Ok(format!("{head}-{}", &hash[..24]))
}
fn save(path: &Path, row: &V, interrupted: &std::sync::atomic::AtomicBool) -> Result<()> {
    let raw = storage::read_external(path, true)?;
    storage::write_external(
        path,
        if path.exists() { Some(&raw) } else { None },
        &row.json(false)?,
        interrupted,
    )
}
fn status(c: &Context<'_>, o: &Options, git: &Git<'_>, directory: &Path) -> Result<Output> {
    let settings =
        oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let lock = oms_lock::Lock::for_file(&directory.join("active"), settings)
        .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let observed = lock.probe();
    let active = match observed {
        oms_lock::Status::Held => true.into(),
        oms_lock::Status::Free => false.into(),
        oms_lock::Status::Unknown => V::Null,
    };
    let code = if o.has("active-exit") {
        match observed {
            oms_lock::Status::Held => 3,
            oms_lock::Status::Free => 0,
            oms_lock::Status::Unknown => 4,
        }
    } else {
        0
    };
    let head = git.commit("HEAD").unwrap_or_default();
    let mut receipts = vec![];
    if directory.exists() {
        for entry in std::fs::read_dir(directory)?.flatten() {
            if entry.path().extension() != Some(OsStr::new("json")) {
                continue;
            }
            if let Ok(raw) = storage::read_external(&entry.path(), false)
                && let Ok(row) = V::parse(&raw)
            {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                receipts.push((string(&row, "sha") == head, modified, row, entry.path()));
            }
        }
    }
    receipts.sort_by_key(|(matches, time, _, _)| (*matches, *time));
    let Some((_, _, mut row, path)) = receipts.pop() else {
        let mut out = if o.has("json") {
            Output::json(&obj([("active", active)]))?
        } else {
            Output::text(format!(
                "land lock: {} (no landing recorded)\n",
                observed.as_str()
            ))
        };
        out.code = if o.has("active-exit") { code } else { 1 };
        return Ok(out);
    };
    let mut matches = false;
    let holder = storage::read_external(&directory.join("active-holder"), true)
        .ok()
        .and_then(|raw| V::parse(&raw).ok());
    if let Some(holder) = holder {
        let pid = holder.get("pid").py().parse::<u32>().unwrap_or(0);
        let alive = oms_command_operations::lock::pid_alive(i64::from(pid))
            && oms_command_operations::lock::process_start(pid).as_deref()
                == holder.get("start").text();
        matches = observed == oms_lock::Status::Held
            && alive
            && holder.get("receipt").text() == path.file_name().and_then(OsStr::to_str)
            && holder.get("sha") == row.get("sha");
        if alive && observed == oms_lock::Status::Held {
            row.set("active_holder", holder.get("sha").clone());
        }
    }
    row.set("active", active);
    if observed == oms_lock::Status::Held {
        row.set("active_matches_receipt", matches);
    }
    let mut out = if o.has("json") {
        Output::json(&row)?
    } else {
        let state = string(&row, "state");
        let mut out = Output::text(format!(
            "land {}: {}\n  active: {}\n",
            string(&row, "sha").chars().take(7).collect::<String>(),
            if state == "running" && !matches {
                "receipt says running; lock does not prove this receipt is active"
            } else {
                &state
            },
            observed.as_str()
        ));
        if state == "superseded" {
            writeln!(
                out.stdout,
                "  successor request: {}",
                string(&row, "successor_request")
            )?;
        }
        out
    };
    out.code = if !o.has("active-exit") && row.get("state").text() == Some("superseded") {
        1
    } else {
        code
    };
    let _ = c;
    Ok(out)
}
fn finish(path: &Path, row: &mut V, state: &str, message: &str, c: &Context<'_>) -> Result<Output> {
    finish_checked(path, row, state, message, c, None)
}
fn finish_checked(
    path: &Path,
    row: &mut V,
    state: &str,
    message: &str,
    c: &Context<'_>,
    checkpoint: Option<&[u8]>,
) -> Result<Output> {
    row.set("state", state);
    row.set("finished_at", now());
    row.set("message", message);
    if let Some(bytes) = checkpoint {
        storage::write_external(path, Some(bytes), &row.json(false)?, c.interrupted)?;
    } else {
        save(path, row, c.interrupted)?;
    }
    *c.relay_row.borrow_mut() = Some(row.clone());
    Ok(Output {
        code: i32::from(!matches!(state, "passed" | "awaiting-merge")),
        stdout: format!("land: {message}\nreceipt: {}\n", path.display()).into_bytes(),
        stderr: vec![],
    })
}
fn relay_card(c: &Context<'_>, o: &Options, result: &Result<Output>) -> Result<()> {
    if !matches!(
        std::env::var("OMS_RELAY").as_deref(),
        Err(_) | Ok("") | Ok("1")
    ) {
        return Ok(());
    }
    let repo = c.repo(o)?;
    let root = c.state(&repo)?;
    if !std::fs::symlink_metadata(root.join(".oms")).is_ok_and(|m| m.file_type().is_dir()) {
        return Ok(());
    }
    storage::ensure_ignore(&root, c.interrupted)?;
    let row = c.relay_row.borrow();
    let row = row.as_ref().unwrap_or(&V::Null);
    // Head and branch come only from the receipt row: live HEAD may have moved,
    // so a run that failed before recording one names no commit.
    let sha = string(row, "sha").chars().take(12).collect::<String>();
    let sha = if sha.len() >= 7
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        sha
    } else {
        String::new()
    };
    let branch = string(row, "head_branch");
    let branch = if branch.len() <= 200
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        branch
    } else {
        String::new()
    };
    let code = match result {
        Ok(output) => output.code,
        Err(error) => error.code,
    };
    let state = if matches!(result, Ok(output) if output.code == 0)
        && matches!(string(row, "state").as_str(), "passed" | "awaiting-merge")
    {
        string(row, "state")
    } else {
        "failed".to_owned()
    };
    let number = row.get("pr_number").py().parse::<usize>().ok();
    let url = string(row, "pr_url");
    let valid_url = url.strip_prefix("https://github.com/").is_some_and(|rest| {
        let parts: Vec<_> = rest.split('/').collect();
        parts.len() == 4
            && parts[2] == "pull"
            && !parts[3].is_empty()
            && parts[3].bytes().all(|b| b.is_ascii_digit())
            && parts[..2].iter().all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
    });
    let message = if sha.is_empty() {
        format!("land {state} (exit {code}); run oms land status")
    } else {
        format!("land {sha} {state} (exit {code}); run oms land status")
    };
    let card = obj([
        ("schema", 1usize.into()),
        ("ended_at", now().into()),
        ("head", sha.into()),
        ("branch", branch.into()),
        ("state", state.into()),
        ("exit", V::from(i64::from(code))),
        ("pr_number", number.map(V::from).unwrap_or(V::Null)),
        ("pr_url", if valid_url { url } else { String::new() }.into()),
        ("message", message.into()),
    ]);
    let path = Path::new(".oms/hooks/relay/land.json");
    let _guard = storage::lock(&root.join(path), c.interrupted)?;
    let old = match std::fs::symlink_metadata(root.join(path)) {
        Ok(_) => Some(storage::read(&root, path, false)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    storage::replace(&root, path, old.as_deref(), &card.json(false)?)?;
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Direct,
    Pr,
    LocalMerge,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Direct => "direct",
            Mode::Pr => "pr",
            Mode::LocalMerge => "local-merge",
        }
    }
}
struct Run<'a> {
    c: &'a Context<'a>,
    o: &'a Options,
    repo: &'a Path,
    git: &'a Git<'a>,
    directory: &'a Path,
    receipt: &'a Path,
    stamp: &'a str,
    request_parts: &'a [&'a str],
    snapshot: &'a Snapshot,
    gate: &'a str,
    destination: &'a str,
    remote: &'a str,
    target: &'a str,
    ci_wait: u64,
    head_branch: &'a str,
    local_ref: Option<&'a str>,
    from_sha: &'a str,
    motivation: &'a str,
    auto_update: bool,
    updates: usize,
}

const MAX_TARGET_UPDATES: usize = 2;
const PR_ANCESTOR_REFUSAL: &str = "remote target is not an ancestor. Fetch the target and merge it into your PR branch, then rerun land with the same head branch.";

struct TargetMoved {
    tip: Option<String>,
    refusal: &'static str,
}

impl TargetMoved {
    fn observe(tip: Option<String>, base: &str, refusal: &'static str) -> Option<Self> {
        (tip.as_deref() != Some(base)).then_some(Self { tip, refusal })
    }

    fn refuse(&self, detail: &str) -> Error {
        if detail.is_empty() {
            Error::new(2, self.refusal)
        } else {
            Error::new(2, format!("{} ({detail})", self.refusal))
        }
    }
}

struct HeadUpdate {
    snapshot: Snapshot,
    branch: String,
    local_ref: String,
    destination: String,
    gate: String,
    tip: String,
    request: String,
    evidence: V,
}

enum Pass {
    Done(Output),
    Updated(Box<HeadUpdate>),
}

fn requests_target_update(o: &Options, ci_wait: u64) -> bool {
    o.has("pr")
        && o.has("merge")
        && ci_wait > 0
        && matches!(o.value("merge-method", "squash"), "squash" | "merge")
}

fn receipt_has_completed_or_requested_merge(row: &V) -> bool {
    row.get("state").text() == Some("passed")
        || (row.has("merge") && row.get("merge").get("state").text() != Some("not-requested"))
}

fn merge_in_progress(git: &Git<'_>) -> Result<bool> {
    let path = git
        .repo
        .join(git.text(&["rev-parse", "--git-path", "MERGE_HEAD"])?);
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

const UNFINISHED_MERGE: &str = "unfinished merge; run git merge --abort before retrying land";

fn restore_merge_index(path: &Path, observed: &str, original: &[u8]) -> Result<()> {
    let lock = path.with_file_name("index.lock");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)?;
    let result = (|| {
        require(
            digest(&storage::read_external(path, false)?) == observed,
            "index changed after merge rollback",
        )?;
        file.set_permissions(std::fs::metadata(path)?.permissions())?;
        file.write_all(original)?;
        file.sync_all()?;
        std::fs::rename(&lock, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(lock);
    }
    result
}

// Preserve the full tracked-state fingerprint separately from permission bits.
fn tracked_modes(git: &Git<'_>) -> Result<(String, Vec<(PathBuf, u32)>)> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let mut proof = git.run(&["ls-files", "--stage", "-z"])?;
    let paths = git.run(&["ls-files", "-z"])?;
    let mut modes = Vec::new();
    for raw in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(OsStr::from_bytes(raw));
        proof.extend_from_slice(raw);
        proof.push(0);
        let full = git.repo.join(&path);
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() => {
                proof.extend_from_slice(b"file\0");
                proof.extend_from_slice(digest(&storage::read(git.repo, &path, false)?).as_bytes());
                require(
                    oms_reference::storage::generation(&meta, &std::fs::symlink_metadata(&full)?),
                    "tracked file changed while fingerprinting",
                )?;
                modes.push((path, meta.mode() & 0o777));
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                proof.extend_from_slice(b"link\0");
                proof.extend_from_slice(std::fs::read_link(full)?.as_os_str().as_bytes());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                proof.extend_from_slice(b"missing")
            }
            Ok(_) => return Err(Error::new(2, "unsupported tracked file type")),
            Err(e) => return Err(e.into()),
        }
        proof.push(0);
    }
    Ok((digest(&proof), modes))
}

fn restore_tracked_mode(repo: &Path, path: &Path, mode: u32) -> Result<()> {
    use rustix::fs::{self, Mode, OFlags};
    use std::os::unix::fs::PermissionsExt;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir =
        fs::open(repo, dir_flags, Mode::empty()).map_err(|e| Error::new(2, e.to_string()))?;
    let mut parts = path.components().peekable();
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(name) = part else {
            return Err(Error::new(2, "invalid tracked path"));
        };
        if parts.peek().is_some() {
            dir = fs::openat(&dir, name, dir_flags, Mode::empty())
                .map_err(|e| Error::new(2, e.to_string()))?;
        } else {
            let fd = fs::openat(
                &dir,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| Error::new(2, e.to_string()))?;
            let file = std::fs::File::from(fd);
            require(file.metadata()?.is_file(), "tracked file is not regular")?;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn rollback_target_update(
    git: &Git<'_>,
    before: &Snapshot,
    local_ref: &str,
    tip: &str,
    index: &Path,
    index_bytes: &[u8],
    tracked_before: &(String, Vec<(PathBuf, u32)>),
) -> Result<()> {
    // Cleanup must run even when the merge was cancelled. Git may have already
    // committed the merge by the time it stopped the process.
    let cleanup = AtomicBool::new(false);
    let git = Git::new(git.repo, &cleanup);
    require(
        git.text(&["symbolic-ref", "HEAD"])? == local_ref
            && (!merge_in_progress(&git)? || git.commit("MERGE_HEAD")? == tip),
        "another checkout or merge is active",
    )?;
    let _ = git.run(&["merge", "--abort"]);
    require(
        !merge_in_progress(&git)? && git.text(&["symbolic-ref", "HEAD"])? == local_ref,
        "merge state or branch changed during rollback",
    )?;
    let head = git.commit("HEAD")?;
    if head != before.head {
        require(
            git.text(&["rev-list", "--parents", "-1", "HEAD"])?
                == format!("{head} {} {tip}", before.head),
            "HEAD changed outside the target update",
        )?;
        git.run(&["reset", "--keep", &before.head])?;
    }
    let mut restored = git.snapshot()?;
    let observed = std::mem::replace(&mut restored.index_sha256, before.index_sha256.clone());
    if restored.tracked_sha256 != before.tracked_sha256 {
        let (proof, modes) = tracked_modes(&git)?;
        let mut without_modes = restored.clone();
        without_modes.tracked_sha256 = before.tracked_sha256.clone();
        require(
            without_modes == *before
                && proof == tracked_before.0
                && modes
                    .iter()
                    .map(|(p, _)| p)
                    .eq(tracked_before.1.iter().map(|(p, _)| p)),
            "tracked checkout changed during rollback",
        )?;
        for ((path, current), (_, original)) in modes.iter().zip(&tracked_before.1) {
            if current != original {
                restore_tracked_mode(git.repo, path, *original)?;
            }
        }
        restored = git.snapshot()?;
        require(
            restored.index_sha256 == observed,
            "index changed during mode restoration",
        )?;
        restored.index_sha256 = before.index_sha256.clone();
    }
    require(
        restored == *before,
        "tracked checkout changed during rollback",
    )?;
    if observed != before.index_sha256 {
        // Snapshot's write-tree can refresh index cache records. Restore the
        // captured bytes only after proving the index tree and files match.
        restore_merge_index(index, &observed, index_bytes)?;
    }
    require(
        storage::read_external(index, false)? == index_bytes
            && digest(index_bytes) == before.index_sha256,
        "pre-merge index was not restored",
    )
}

fn protect_local_paths(git: &Git<'_>, tip: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let paths = match git.text(&["merge-base", "HEAD", tip]) {
        Ok(base) => git.run(&["diff", "--name-only", "-z", &base, tip])?,
        Err(_) => git.run(&["ls-tree", "-r", "--name-only", "-z", tip])?,
    };
    let tracked = git.run(&["ls-files", "-z"])?;
    let tracked: std::collections::HashSet<&[u8]> = tracked.split(|b| *b == 0).collect();
    for raw in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = Path::new(OsStr::from_bytes(raw));
        for component in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            let bytes = component.as_os_str().as_bytes();
            match std::fs::symlink_metadata(git.repo.join(component)) {
                Ok(meta)
                    if (component == path && !tracked.contains(bytes))
                        || (component != path && !meta.is_dir()) =>
                {
                    return Err(Error::new(
                        2,
                        "target update would overwrite an untracked or ignored path",
                    ));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

fn update_target(
    r: &Run<'_>,
    row: &mut V,
    moved: TargetMoved,
    checkpoint: Option<&[u8]>,
    preflight: bool,
) -> Result<Pass> {
    if !r.auto_update {
        return Err(moved.refuse(""));
    }
    if r.updates >= MAX_TARGET_UPDATES {
        return Err(moved.refuse("target kept moving"));
    }
    let Some(tip) = moved.tip.as_deref() else {
        return Err(moved.refuse(""));
    };
    if receipt_has_completed_or_requested_merge(row) {
        return Err(moved.refuse("existing receipt records a completed or requested merge"));
    }
    let git = r.git;
    let flag = r.c.interrupted;
    require(!merge_in_progress(git)?, UNFINISHED_MERGE)?;
    let local_ref = r
        .local_ref
        .ok_or_else(|| moved.refuse("target update requires a checked-out branch"))?;
    // Fetch from the checked push destination. The observed object is the only
    // merge input even if the target ref advances again while fetching.
    let prepare = (|| {
        git.transport_safe()?;
        require(
            git.text(&["remote", "get-url", "--push", "--all", r.remote])? == r.destination,
            "push destination changed before target update",
        )?;
        if git.commit(tip).is_err() {
            git.run(&["fetch", "--no-tags", "--", r.destination, tip])?;
        }
        require(
            git.commit(tip)? == tip,
            "target update needs the exact observed commit",
        )?;
        require(
            !git.succeeds(&["merge-base", "--is-ancestor", &r.snapshot.head, tip])?,
            "HEAD is already contained in the target",
        )?;
        protect_local_paths(git, tip)?;
        if preflight {
            pr_motivation(git, r.o, tip, &r.snapshot.head)?;
        }
        let before = git.snapshot()?;
        require(
            !merge_in_progress(git)?
                && git.text(&["symbolic-ref", "HEAD"])? == local_ref
                && before == *r.snapshot
                && before.tree == git.text(&["rev-parse", "HEAD^{tree}"])?,
            "checkout changed before target update",
        )?;
        git.tracked_clean()?;
        let index = r
            .repo
            .join(git.text(&["rev-parse", "--git-path", "index"])?);
        let bytes = storage::read_external(&index, false)?;
        require(
            digest(&bytes) == before.index_sha256,
            "index changed before target update",
        )?;
        let tracked = tracked_modes(git)?;
        require(
            git.snapshot()? == before,
            "checkout changed before target update",
        )?;
        Ok::<_, Error>((before, index, bytes, tracked))
    })();
    let (before, index, index_bytes, tracked_before) =
        prepare.map_err(|e| moved.refuse(&e.message))?;
    let prior = if let Some(bytes) = checkpoint {
        bytes.to_vec()
    } else {
        row.json(false)?
    };
    let mut environment = global_git_config();
    environment.push(("OMS_LAND_ACTIVE".into(), "1".into()));
    let mut replacement = None;
    let updated: Result<_> = (|| {
        let merge = crate::git::process(
            r.repo,
            ["git", "merge", "--no-edit", "--no-ff", tip]
                .map(std::ffi::OsString::from)
                .to_vec(),
            None,
            Duration::from_secs(3600),
            flag,
            &environment,
        )?;
        require(merge.code == 0, "target update merge failed")?;
        git.transport_safe()?;
        git.tracked_clean()?;
        let snapshot = git.snapshot()?;
        let head = &snapshot.head;
        require(
            !merge_in_progress(git)?
                && git.text(&["symbolic-ref", "HEAD"])? == local_ref
                && snapshot.tree == git.text(&["rev-parse", "HEAD^{tree}"])?
                && git.text(&["rev-list", "--parents", "-1", "HEAD"])?
                    == format!("{head} {} {tip}", before.head),
            "target update did not produce the expected merge commit",
        )?;
        let request = request_stamp(head, r.request_parts)?;
        let evidence = obj([
            ("commit", head.as_str().into()),
            (
                "parents",
                V::Arr(vec![before.head.clone().into(), tip.into()]),
            ),
            ("predecessor_request", r.stamp.into()),
        ]);
        row.set("state", "superseded");
        row.set("finished_at", now());
        row.set(
            "message",
            format!("target moved; superseded by request {request}"),
        );
        row.set("successor_request", request.clone());
        row.set("superseding_update", evidence.clone());
        let bytes = replacement.insert(row.json(false)?);
        storage::write_external(r.receipt, Some(&prior), bytes, flag)?;
        crate::check(flag)?;
        Ok(Box::new(HeadUpdate {
            snapshot,
            branch: r.head_branch.into(),
            local_ref: local_ref.into(),
            destination: r.destination.into(),
            gate: r.gate.into(),
            tip: tip.into(),
            request,
            evidence,
        }))
    })();
    match updated {
        Ok(update) => Ok(Pass::Updated(update)),
        Err(error) => {
            let restore_receipt: Result<()> = (|| {
                if let Some(bytes) = replacement {
                    let current = storage::read_external(r.receipt, false)?;
                    if current == bytes {
                        // A failed fsync or cancellation after rename can leave
                        // the superseded receipt published despite the error.
                        storage::write_external(
                            r.receipt,
                            Some(&bytes),
                            &prior,
                            &AtomicBool::new(false),
                        )?;
                    } else {
                        require(current == prior, "receipt changed during target update")?;
                    }
                }
                Ok(())
            })();
            rollback_target_update(git, &before, local_ref, tip, &index, &index_bytes, &tracked_before)
                .map_err(|_| moved.refuse("target update rollback could not be verified; inspect the checkout before retrying"))?;
            restore_receipt.map_err(|_| {
                moved.refuse("target update receipt rollback could not be verified")
            })?;
            Err(moved.refuse(&format!(
                "target update failed and was rolled back: {}",
                error.message
            )))
        }
    }
}

/// Runs the gate in a detached sandbox at the exact HEAD (local-merge: at the
/// merge result). `Some` is a finished failure; nothing has been published.
fn gate_phase(r: &Run<'_>, row: &mut V, updated_base: Option<&str>) -> Result<Option<Output>> {
    let (c, repo, snapshot, gate) = (r.c, r.repo, r.snapshot, r.gate);
    if row.get("gate").get("rc") == &0usize.into() {
        return Ok(None);
    }
    let scratch = Sandbox::new(repo, &snapshot.head, c.interrupted)?;
    if let Some(base) = updated_base {
        // The default gate reads this tracking ref in its private clone.
        Git::new(&scratch.path, c.interrupted).run(&[
            "update-ref",
            &format!("refs/remotes/{}/{}", r.remote, r.target),
            base,
        ])?;
    }
    let mut merged_tree = None;
    if !r.from_sha.is_empty() {
        let sandbox = Git::new(&scratch.path, c.interrupted);
        sandbox.raw(&[
            OsStr::new("fetch"),
            OsStr::new("--quiet"),
            OsStr::new("--no-tags"),
            OsStr::new("--"),
            repo.as_os_str(),
            OsStr::new(r.from_sha),
        ])?;
        let merge = sandbox.output(
            &[
                OsStr::new("-c"),
                OsStr::new("user.name=oms-land"),
                OsStr::new("-c"),
                OsStr::new("user.email=oms-land@oms.invalid"),
                OsStr::new("merge"),
                OsStr::new("--no-edit"),
                OsStr::new(r.from_sha),
            ],
            None,
            &[],
        )?;
        if merge.code != 0 {
            return finish(
                r.receipt,
                row,
                "failed",
                "merge does not apply cleanly; nothing merged",
                c,
            )
            .map(Some);
        }
        merged_tree = Some(sandbox.text(&["rev-parse", "HEAD^{tree}"])?);
    }
    let result = admission::verify(c, &scratch.path, gate, Duration::from_secs(3600))?;
    let log = [result.stdout, result.stderr].concat();
    let log = if crate::safe(&log).is_ok() {
        log
    } else {
        b"[gate output withheld]\n".to_vec()
    };
    let log_path = r.directory.join(format!("{}.log", r.stamp));
    let old = storage::read_external(&log_path, true)?;
    storage::write_external(
        &log_path,
        if log_path.exists() { Some(&old) } else { None },
        &log,
        c.interrupted,
    )?;
    let mut gate_row = row.get("gate").clone();
    gate_row.set("rc", V::Num(result.code.to_string()));
    let words = oms_command_plan::floor::split(gate, false)?;
    let full = words == ["bash", "scripts/check.sh", "--parallel"];
    gate_row.set("scope", if full { "full" } else { "unknown" });
    if let Some(tree) = merged_tree {
        gate_row.set("tree", tree);
    }
    row.set("gate", gate_row);
    save(r.receipt, row, c.interrupted)?;
    if result.code != 0 {
        return finish(r.receipt, row, "failed", "gate failed; nothing pushed", c).map(Some);
    }
    Ok(None)
}
fn gh_exec(r: &Run<'_>, cwd: &Path, args: &[&str]) -> Result<crate::git::Execution> {
    let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| "gh".into());
    crate::git::process(
        cwd,
        std::iter::once(bin)
            .chain(args.iter().map(std::ffi::OsString::from))
            .collect(),
        None,
        Duration::from_secs(60),
        r.c.interrupted,
        &[("GH_HOST".into(), "github.com".into())],
    )
}
fn open_pr(r: &Run<'_>, repo_arg: &str) -> Result<Option<V>> {
    let rows = ci::gh(
        r.c,
        &[
            "pr",
            "list",
            "--repo",
            repo_arg,
            "--head",
            r.head_branch,
            "--base",
            r.target,
            "--state",
            "open",
            "--json",
            if r.ci_wait > 0 {
                "number,url,headRefOid,headRefName,baseRefName,isCrossRepository"
            } else {
                "number,url,headRefOid"
            },
            "--limit",
            "2",
        ],
        Duration::from_secs(30),
    )?;
    require(
        matches!(rows, V::Arr(_)) && rows.array().len() <= 1,
        "ambiguous or invalid pull request list",
    )?;
    let pr = rows.array().first();
    if r.ci_wait > 0
        && let Some(pr) = pr
    {
        require(
            pr.get("isCrossRepository") == &V::Bool(false)
                && pr.get("headRefName").text() == Some(r.head_branch)
                && pr.get("baseRefName").text() == Some(r.target),
            "open PR does not match the published source and target",
        )?;
    }
    Ok(pr.cloned())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum BranchDelete {
    Allow,
    SkipDependentPr,
    SkipUnverified,
}
impl BranchDelete {
    fn allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
    fn skip_reason(self) -> Option<&'static str> {
        match self {
            Self::Allow => None,
            Self::SkipDependentPr => Some("open-dependent-pr"),
            Self::SkipUnverified => Some("dependent-pr-check-unverified"),
        }
    }
    fn notice(self) -> Option<&'static str> {
        match self {
            Self::Allow => None,
            Self::SkipDependentPr => Some("branch kept: open dependent PR targets head branch"),
            Self::SkipUnverified => Some("branch kept: dependent PR check unverified"),
        }
    }
}
fn dependent_pr_delete_guard(r: &Run<'_>, repo_arg: &str) -> Result<BranchDelete> {
    match ci::gh(
        r.c,
        &[
            "pr",
            "list",
            "--repo",
            repo_arg,
            "--state",
            "open",
            "--base",
            r.head_branch,
            "--json",
            "number",
            "--limit",
            "1",
        ],
        Duration::from_secs(30),
    ) {
        Ok(V::Arr(rows)) if rows.is_empty() => Ok(BranchDelete::Allow),
        Ok(V::Arr(_)) => Ok(BranchDelete::SkipDependentPr),
        Ok(_) => Ok(BranchDelete::SkipUnverified),
        Err(_) => {
            crate::check(r.c.interrupted)?;
            Ok(BranchDelete::SkipUnverified)
        }
    }
}
fn pr_text(base: &str, r: &Run<'_>, scope: &str) -> Result<(String, String)> {
    let range = format!("{base}..{}", r.snapshot.head);
    let log = r
        .git
        .text(&["log", "--format=%h %s", "--max-count=50", &range])?;
    let count: usize = r
        .git
        .text(&["rev-list", "--count", &range])?
        .parse()
        .map_err(|_| Error::new(2, "unreadable commit count"))?;
    let subjects = r.git.text(&[
        "log",
        "--first-parent",
        "--no-merges",
        "--format=%s",
        "--max-count=50",
        &range,
    ])?;
    let newest = subjects
        .lines()
        .next()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| r.motivation.lines().find(|s| !s.trim().is_empty()))
        .unwrap_or("land");
    let mut title: String = newest.chars().take(100).collect();
    if count > 1 {
        title += &format!(" (+{} more)", count - 1);
    }
    let body = format!(
        "## Summary\n{}\n\n## Motivation\n{}\n\n## Verification\n- Local gate `{}` passed (scope {scope}) on exact head {}\n- `oms land` checks CI on this exact head; merge only with explicit `--merge`\n",
        log.lines()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        r.motivation,
        r.gate,
        r.snapshot.head
    );
    crate::safe(title.as_bytes())?;
    crate::safe(body.as_bytes())?;
    Ok((title, body))
}
enum PrCi {
    Passed(&'static str),
    Failed(String),
}

/// Samples exact-head PR CI for up to `wait`. Only `row` in memory changes:
/// the caller owns the receipt compare-and-swap.
fn observe_pr_ci(
    r: &Run<'_>,
    row: &mut V,
    wait: Duration,
    observation: Option<&pr_observation::Checkpoint>,
) -> Result<PrCi> {
    let (c, snapshot, head_branch) = (r.c, r.snapshot, r.head_branch);
    let deadline = Instant::now() + wait;
    let mut scope = "unknown";
    let conclusion = loop {
        crate::check(c.interrupted)?;
        let mut found = None;
        for event in ["pull_request", "push"] {
            match ci::latest(c, r.destination, &snapshot.head, head_branch, event) {
                Ok(Some(run)) => {
                    found = Some(run);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    crate::check(c.interrupted)?;
                    row.set("ci", obj([("conclusion", "unknown".into())]));
                    return Ok(PrCi::Failed(format!(
                        "CI unknown: {e}; pull request left open"
                    )));
                }
            }
        }
        let state = if let Some(run) = found {
            let id = run.get("databaseId");
            let mut state = ci::conclusion(&run, &snapshot.head)?;
            if state == "success" {
                let detail = ci::run_details(c, r.destination, id, &snapshot.head);
                crate::check(c.interrupted)?;
                let confirmed = detail.and_then(|detail| {
                    let state = ci::conclusion(&detail, &snapshot.head)?;
                    if state == "success" {
                        scope = ci::successful_scope(&detail, &snapshot.head)?;
                    }
                    Ok(state)
                });
                state = match confirmed {
                    Ok(state) => state,
                    Err(e) => {
                        row.set(
                            "ci",
                            obj([("conclusion", "unknown".into()), ("run_id", id.clone())]),
                        );
                        return Ok(PrCi::Failed(format!(
                            "CI detail unavailable: {e}; pull request left open"
                        )));
                    }
                };
            }
            row.set(
                "ci",
                obj([("conclusion", state.clone().into()), ("run_id", id.clone())]),
            );
            state
        } else {
            row.set("ci", obj([("conclusion", "pending".into())]));
            "pending".into()
        };
        if state != "pending" || Instant::now() >= deadline {
            break state;
        }
        let next = deadline.min(Instant::now() + Duration::from_secs(10));
        if let Some(checkpoint) = observation {
            while Instant::now() < next {
                crate::check(c.interrupted)?;
                checkpoint.check_legacy_claim()?;
                std::thread::park_timeout(
                    Duration::from_millis(250).min(next.saturating_duration_since(Instant::now())),
                );
            }
        } else {
            std::thread::park_timeout(next.saturating_duration_since(Instant::now()));
        }
    };
    if conclusion != "success" {
        return Ok(PrCi::Failed(format!(
            "CI {conclusion}; pull request left open and unmerged; retry --wait"
        )));
    }
    let mut ci_row = row.get("ci").clone();
    ci_row.set("scope", scope);
    row.set("ci", ci_row);
    Ok(PrCi::Passed(scope))
}

fn land_pr(r: &Run<'_>, row: &mut V, base: &str, active: oms_lock::Guard) -> Result<Pass> {
    let (c, o, git, snapshot, target, head_branch) =
        (r.c, r.o, r.git, r.snapshot, r.target, r.head_branch);
    if base == snapshot.head {
        row.set("ci", obj([("conclusion", "skipped".into())]));
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "nothing-to-land".into()),
            ]),
        );
        return finish(
            r.receipt,
            row,
            "passed",
            "remote target already contains HEAD; nothing to land",
            c,
        )
        .map(Pass::Done);
    }
    let repo_arg = ci::query_repo(r.destination)?;
    let method = o.value("merge-method", "squash");
    let (title, body) = pr_text(base, r, &string(row.get("gate"), "scope"))?;
    row.set("head_branch", head_branch);
    save(r.receipt, row, c.interrupted)?;
    {
        let _guard = c
            .services
            .publication_guard(&c.state(r.repo)?, c.interrupted)?;
        require(git.snapshot()? == *snapshot, "tree moved before push")?;
        if let Some(moved) = TargetMoved::observe(
            crate::review::remote_tip(git, r.destination, target)?,
            base,
            "remote target changed before push",
        ) {
            return update_target(r, row, moved, None, false);
        }
        git.transport_safe()?;
        let existing = crate::review::remote_tip(git, r.destination, head_branch)?;
        if let Some(tip) = existing.as_deref().filter(|t| *t != snapshot.head) {
            let ancestor = git.output(
                &[
                    OsStr::new("merge-base"),
                    OsStr::new("--is-ancestor"),
                    OsStr::new(tip),
                    OsStr::new(&snapshot.head),
                ],
                None,
                &[],
            )?;
            if ancestor.code != 0 {
                return finish(
                    r.receipt,
                    row,
                    "failed",
                    "remote head branch has diverged from local HEAD. Fetch and merge the existing remote PR head into your local branch, then rerun land without a force push.",
                    c,
                ).map(Pass::Done);
            }
        }
        if existing.as_deref() != Some(&snapshot.head) {
            let refspec = format!("{}:refs/heads/{head_branch}", snapshot.head);
            let lease = existing
                .as_ref()
                .map(|tip| format!("--force-with-lease=refs/heads/{head_branch}:{tip}"));
            let mut args = vec![OsStr::new("push"), OsStr::new("--no-verify")];
            args.extend(lease.as_deref().map(OsStr::new));
            args.extend([OsStr::new(r.remote), OsStr::new(&refspec)]);
            let result = git.output(&args, None, &[])?;
            row.set(
                "push",
                obj([
                    ("rc", V::Num(result.code.to_string())),
                    ("base", base.into()),
                    ("head_branch", head_branch.into()),
                    ("moved", 1usize.into()),
                ]),
            );
            save(r.receipt, row, c.interrupted)?;
            if result.code != 0 {
                return finish(
                    r.receipt,
                    row,
                    "failed",
                    "branch push failed or outcome unknown; inspect remote before retrying",
                    c,
                )
                .map(Pass::Done);
            }
        }
    }
    let scratch = storage::Temp::new("oh-my-setting-land-pr")?;
    let mut pr = open_pr(r, &repo_arg)?;
    if pr.is_none() {
        let body_path = scratch.path.join("body.md");
        std::fs::write(&body_path, &body)?;
        let created = gh_exec(
            r,
            &scratch.path,
            &[
                "pr",
                "create",
                "--repo",
                &repo_arg,
                "--base",
                target,
                "--head",
                head_branch,
                "--title",
                &title,
                "--body-file",
                &body_path.to_string_lossy(),
            ],
        )?;
        if created.code != 0 {
            return finish(
                r.receipt,
                row,
                "failed",
                "branch pushed; pull request creation failed",
                c,
            )
            .map(Pass::Done);
        }
        pr = open_pr(r, &repo_arg)?;
    }
    for _ in 0..5 {
        if pr
            .as_ref()
            .is_some_and(|p| p.get("headRefOid").text() == Some(&snapshot.head))
        {
            break;
        }
        std::thread::park_timeout(Duration::from_secs(2));
        pr = open_pr(r, &repo_arg)?;
    }
    let Some(pr) = pr.filter(|p| {
        p.get("headRefOid").text() == Some(&snapshot.head) && p.get("number").integer(true)
    }) else {
        return finish(
            r.receipt,
            row,
            "failed",
            "no open pull request at the exact HEAD; nothing merged",
            c,
        )
        .map(Pass::Done);
    };
    let number = pr.get("number").py();
    row.set("pr_number", pr.get("number").clone());
    row.set("pr_url", pr.get("url").clone());
    save(r.receipt, row, c.interrupted)?;
    let mut active = Some(active);
    let mut prior = None;
    if r.ci_wait == 0 {
        row.set(
            "ci",
            obj([
                ("conclusion", "skipped".into()),
                ("scope", "unknown".into()),
            ]),
        );
    } else {
        let checkpoint = pr_observation::Checkpoint::begin(r, row, &mut active)?;
        let observed = observe_pr_ci(r, row, Duration::from_secs(r.ci_wait), Some(&checkpoint))?;
        crate::check(c.interrupted)?;
        let pr_observation::Reentry {
            guard,
            receipt: bytes,
            moved,
        } = checkpoint.reenter(r, &pr, base)?;
        // The repository guard stays alive through every later write and merge.
        guard
            .check()
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        active = Some(guard);
        if let Some(moved) = moved {
            return update_target(r, row, moved, Some(&bytes), false);
        }
        prior = Some(bytes);
        if let PrCi::Failed(reason) = observed {
            return finish_checked(r.receipt, row, "failed", &reason, c, prior.as_deref())
                .map(Pass::Done);
        }
    }
    if !o.has("merge") {
        row.set("merge", obj([("state", "not-requested".into())]));
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "awaiting-merge".into()),
            ]),
        );
        let message = format!(
            "pull request #{number} open: {}; CI {}; not merged \u{2014} pass --merge only when the user asked to merge",
            string(&pr, "url"),
            string(row.get("ci"), "conclusion")
        );
        return finish_checked(
            r.receipt,
            row,
            "awaiting-merge",
            &message,
            c,
            prior.as_deref(),
        )
        .map(Pass::Done);
    }
    let checkpoint = prior.ok_or_else(|| Error::new(2, "merge requires a CI checkpoint"))?;
    let delete_branch = dependent_pr_delete_guard(r, &repo_arg)?;
    let flag = format!("--{method}");
    let mut merge_state = obj([
        ("method", method.into()),
        ("state", "requested".into()),
        ("delete_branch", delete_branch.allowed().into()),
    ]);
    if let Some(reason) = delete_branch.skip_reason() {
        merge_state.set("delete_branch_skipped", reason);
    }
    // A neutral cwd keeps `--delete-branch` from touching this checkout.
    let mut merge_args = vec!["pr", "merge", &number, "--repo", &repo_arg, &flag];
    if delete_branch.allowed() {
        merge_args.push("--delete-branch");
    }
    merge_args.extend(["--match-head-commit", &snapshot.head]);
    let ci_scope;
    let merged = {
        // Same writer -> publication lock order as the initial push. A sibling
        // which appeared during CI causes a retry, never a wait under both locks.
        let state = c.state(r.repo)?;
        let _publication = c.services.publication_guard(&state, c.interrupted)?;
        if !o.has("ignore-siblings") {
            c.services
                .wait_siblings(r.repo, Duration::ZERO, c.interrupted)?;
            if state != r.repo {
                c.services
                    .wait_siblings(&state, Duration::ZERO, c.interrupted)?;
            }
        }
        // The observed verdict predates writer reentry; a rerun may have replaced it.
        let fresh = observe_pr_ci(r, row, Duration::ZERO, None)?;
        let moved = pr_observation::validate_destination(r, &pr, base)?;
        let writer = active
            .as_ref()
            .ok_or_else(|| Error::new(2, "missing publication lock"))?;
        writer
            .check()
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        if let Some(moved) = moved {
            return update_target(r, row, moved, Some(&checkpoint), false);
        }
        ci_scope = match fresh {
            PrCi::Passed(scope) => scope,
            PrCi::Failed(reason) => {
                return finish_checked(r.receipt, row, "failed", &reason, c, Some(&checkpoint))
                    .map(Pass::Done);
            }
        };
        row.set("merge", merge_state);
        storage::write_external(
            r.receipt,
            Some(&checkpoint),
            &row.json(false)?,
            c.interrupted,
        )?;
        crate::check(c.interrupted)?;
        writer
            .check()
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
        gh_exec(r, &scratch.path, &merge_args)?
    };
    if merged.code != 0 {
        return finish(
            r.receipt,
            row,
            "failed",
            "merge refused or outcome unknown; pull request left open",
            c,
        )
        .map(Pass::Done);
    }
    let view = ci::gh(
        c,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &repo_arg,
            "--json",
            "state,mergeCommit",
        ],
        Duration::from_secs(30),
    );
    let (state, commit) = match &view {
        Ok(v) => (
            string(v, "state"),
            v.get("mergeCommit").get("oid").clone().or(""),
        ),
        Err(_) => (String::from("UNKNOWN"), "".into()),
    };
    let mut merge_row = obj([
        ("method", method.into()),
        ("state", state.clone().into()),
        ("commit", commit),
        ("delete_branch", delete_branch.allowed().into()),
    ]);
    if let Some(reason) = delete_branch.skip_reason() {
        merge_row.set("delete_branch_skipped", reason);
    }
    row.set("merge", merge_row);
    save(r.receipt, row, c.interrupted)?;
    if state != "MERGED" {
        return finish(
            r.receipt,
            row,
            "failed",
            "merge requested; merged state not confirmed",
            c,
        )
        .map(Pass::Done);
    }
    let fetched = git.run(&[
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "--no-tags",
        "--",
        r.remote,
        &format!("+refs/heads/{target}:refs/remotes/{}/{target}", r.remote),
    ]);
    if fetched.is_err() {
        return finish(
            r.receipt,
            row,
            "failed",
            "merged; fetching the target failed",
            c,
        )
        .map(Pass::Done);
    }
    let local_full = row.get("gate").get("scope").text() == Some("full");
    let merge_commit = row
        .get("merge")
        .get("commit")
        .text()
        .unwrap_or("")
        .to_owned();
    // A new merge commit carries the CI verdict of the PR head only when it
    // holds the same tree directly on top of the base this land checked.
    let mut covered = merge_commit.is_empty() || merge_commit == snapshot.head;
    if !covered && (crate::hex(&merge_commit, 40) || crate::hex(&merge_commit, 64)) {
        let rev = |spec: String| git.text(&["rev-parse", "--verify", "--quiet", &spec]).ok();
        let tree_equal = rev(format!("{merge_commit}^{{tree}}"))
            .is_some_and(|t| Some(t) == rev(format!("{}^{{tree}}", snapshot.head)));
        let mut merge_row = row.get("merge").clone();
        merge_row.set("tree_equal", tree_equal);
        row.set("merge", merge_row);
        covered = tree_equal
            && rev(format!("{merge_commit}^1")).as_deref() == Some(base)
            && git
                .succeeds(&[
                    "merge-base",
                    "--is-ancestor",
                    &merge_commit,
                    &format!("refs/remotes/{}/{target}", r.remote),
                ])
                .unwrap_or(false);
    }
    if o.has("no-update") {
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "disabled".into())]),
        );
    } else if !covered {
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "merge-commit-pending-ci".into()),
            ]),
        );
    } else if ci_scope == "full" || local_full {
        let update = c.services.after_land(r.repo, row, c.interrupted)?;
        row.set("update", update);
        if row.get("update").get("rc") != &0usize.into() {
            return finish(
                r.receipt,
                row,
                "failed",
                "merged; installation failed or was deferred",
                c,
            )
            .map(Pass::Done);
        }
    } else {
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "full-ci-required".into()),
            ]),
        );
    }
    save(r.receipt, row, c.interrupted)?;
    let message = match delete_branch.notice() {
        Some(notice) => format!("merged PR #{number} ({method}) after exact-SHA CI ({notice})"),
        None => format!("merged PR #{number} ({method}) after exact-SHA CI"),
    };
    let mut output = finish(r.receipt, row, "passed", &message, c)?;
    tail(r, &mut output)?;
    Ok(Pass::Done(output))
}
/// `oms/<type>/<slug>` from a `<type>: <description>` subject, else `oms/land-<sha12>`.
fn default_head_branch(subject: &str, head: &str) -> String {
    const TYPES: [&str; 8] = [
        "feat", "fix", "refactor", "chore", "docs", "test", "perf", "ci",
    ];
    if let Some((kind, description)) = subject.split_once(": ")
        && TYPES.contains(&kind)
    {
        let mut slug = String::new();
        for c in description.chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        slug.truncate(48);
        let slug = slug.trim_end_matches('-');
        if !slug.is_empty() {
            return format!("oms/{kind}/{slug}");
        }
    }
    format!("oms/land-{}", &head[..12])
}
/// Every changed path is Markdown and at most two lines changed; binary refuses.
fn trivial_docs(git: &Git<'_>, base: &str, head: &str) -> Result<bool> {
    let raw = git.run(&["diff", "--numstat", "-z", "--no-renames", base, head])?;
    let mut lines = 0u64;
    for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let mut fields = record.splitn(3, '\t');
        let (Some(Ok(added)), Some(Ok(deleted)), Some(path)) = (
            fields.next().map(str::parse::<u64>),
            fields.next().map(str::parse::<u64>),
            fields.next(),
        ) else {
            return Ok(false);
        };
        if !path.ends_with(".md") {
            return Ok(false);
        }
        lines += added + deleted;
    }
    Ok(lines <= 2)
}
fn global_git_config() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let candidates = [
        std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from),
        home.as_ref().map(|h| h.join(".gitconfig")),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|x| PathBuf::from(x).join("git/config"))
            .or_else(|| home.as_ref().map(|h| h.join(".config/git/config"))),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .map(|p| vec![("GIT_CONFIG_GLOBAL".into(), p.into_os_string())])
        .unwrap_or_default()
}
fn land_local(r: &Run<'_>, row: &mut V) -> Result<Output> {
    let (c, git, snapshot) = (r.c, r.git, r.snapshot);
    row.set("merged_from", r.from_sha);
    row.set("push", obj([("rc", "skipped".into())]));
    row.set("ci", obj([("conclusion", "skipped".into())]));
    row.set(
        "update",
        obj([("rc", "skipped".into()), ("reason", "local-merge".into())]),
    );
    save(r.receipt, row, c.interrupted)?;
    require(
        git.snapshot()? == *snapshot,
        "tree moved before the local merge",
    )?;
    let mut environment = global_git_config();
    environment.push(("OMS_LAND_ACTIVE".into(), "1".into()));
    let before_head = git.commit("HEAD")?;
    // Hooks stay enabled here: a pre-merge-commit guard accepts only this gated merge.
    let merge = crate::git::process(
        r.repo,
        ["git", "merge", "--no-edit", r.from_sha]
            .iter()
            .map(std::ffi::OsString::from)
            .collect(),
        None,
        Duration::from_secs(3600),
        c.interrupted,
        &environment,
    )?;
    if merge.code != 0 {
        let _ = git.output(&[OsStr::new("merge"), OsStr::new("--abort")], None, &[]);
        return finish(
            r.receipt,
            row,
            "failed",
            "merge refused, conflicted or lacked a Git identity; aborted",
            c,
        );
    }
    let head = git.commit("HEAD")?;
    row.set("merged_head", head);
    save(r.receipt, row, c.interrupted)?;
    let tree = match git.text(&["rev-parse", "HEAD^{tree}"]) {
        Ok(t) => t,
        Err(e) => {
            let _ = git.output(
                &[
                    OsStr::new("reset"),
                    OsStr::new("--hard"),
                    OsStr::new(&before_head),
                ],
                None,
                &[],
            );
            return Err(e);
        }
    };
    if row
        .get("gate")
        .get("tree")
        .text()
        .is_some_and(|t| t != tree)
    {
        let _ = git.output(
            &[
                OsStr::new("reset"),
                OsStr::new("--hard"),
                OsStr::new(&before_head),
            ],
            None,
            &[],
        );
        return finish(
            r.receipt,
            row,
            "failed",
            "merged tree differs from the gated tree; rolled back the local merge",
            c,
        );
    }
    finish(
        r.receipt,
        row,
        "passed",
        "gated merge completed locally; nothing pushed",
        c,
    )
}

fn pr_motivation(git: &Git<'_>, o: &Options, base: &str, head: &str) -> Result<String> {
    let motivation = if o.has("why") {
        o.get("why").trim().to_owned()
    } else {
        git.text(&[
            "log",
            "--format=%b%x00",
            "--max-count=50",
            &format!("{base}..{head}"),
        ])?
        .split('\0')
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
    };
    require(
        !motivation.is_empty(),
        "a pull request needs a motivation: pass --why TEXT or write a commit body",
    )?;
    crate::safe(motivation.as_bytes())?;
    Ok(motivation)
}

pub(crate) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    // A harness child is refused before it runs and must not touch the parent's relay.
    let run = o.action("").is_empty() && !o.has("check-manifest") && !crate::child();
    let result = execute_inner(c, o);
    if run {
        let uninterrupted = AtomicBool::new(false);
        let relay_context = Context {
            cwd: c.cwd,
            source: c.source,
            interrupted: &uninterrupted,
            services: c.services,
            relay_row: std::cell::RefCell::new(c.relay_row.borrow().clone()),
        };
        let _ = relay_card(&relay_context, o, &result);
    }
    result
}

fn execute_inner(c: &Context<'_>, o: &Options) -> Result<Output> {
    if o.action("") == "wait" {
        return crate::wait::execute(c, o);
    }
    if o.action("") == "reconcile" {
        return crate::reconcile::execute(c, o);
    }
    if o.has("check-manifest") {
        return crate::manifest::execute(c, o);
    }
    match o.action("") {
        "notes" => return crate::notes::execute(c, o),
        "blame-session" => return crate::attribution::blame(c, o),
        "conflicts" => return conflicts(c, o),
        "batch-preview" | "batch-check" => return crate::batch_preview::execute(c, o),
        "" | "status" => {}
        _ => return Err(Error::new(2, "unknown land action")),
    }
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    let directory = state_directory(&git)?;
    if o.action("") == "status" {
        require(o.words.len() == 1, "unexpected land status argument")?;
        return status(c, o, &git, &directory);
    }
    parent()?;
    let mut previous_update = None;
    let mut progress = Vec::new();
    for updates in 0..=MAX_TARGET_UPDATES {
        match execute_pass(
            c,
            o,
            &repo,
            &git,
            &directory,
            previous_update.as_deref(),
            updates,
        ) {
            Ok(Pass::Done(mut output)) => {
                if !progress.is_empty() {
                    progress.extend(output.stdout);
                    output.stdout = progress;
                }
                return Ok(output);
            }
            Err(e) if !progress.is_empty() => {
                return Ok(Output {
                    code: e.code,
                    stdout: progress,
                    stderr: format!("error: {}; target update successor was not completed; retry land from the updated checkout with the same head branch\n", e.message).into_bytes(),
                });
            }
            Err(e) => return Err(e),
            Ok(Pass::Updated(update)) => {
                // All guards from the previous pass have been released.
                let _ = writeln!(
                    progress,
                    "land: target moved; merged {} into {} as {} (update {} of {MAX_TARGET_UPDATES})",
                    update.tip,
                    update.branch,
                    update.snapshot.head,
                    updates + 1
                );
                previous_update = Some(update);
            }
        }
    }
    unreachable!("target updates are capped inside each pass")
}

fn execute_pass(
    c: &Context<'_>,
    o: &Options,
    repo: &Path,
    git: &Git<'_>,
    directory: &Path,
    previous_update: Option<&HeadUpdate>,
    updates: usize,
) -> Result<Pass> {
    let env_mode = std::env::var("OMS_LAND_MODE").unwrap_or_default();
    require(
        ["", "direct", "pr"].contains(&env_mode.as_str()),
        "OMS_LAND_MODE must be pr or direct",
    )?;
    let ci_wait = o
        .value("ci-wait", "2700")
        .parse::<u64>()
        .map_err(|_| Error::new(2, "--ci-wait must be seconds"))?;
    let sibling_wait = o
        .value("sibling-wait", "1800")
        .parse::<u64>()
        .map_err(|_| Error::new(2, "--sibling-wait must be seconds"))?;
    require(
        !o.has("ci-full") || (ci_wait > 0 && !o.has("no-update")),
        "--ci-full needs --ci-wait above zero and an install",
    )?;
    require(
        !o.has("handoff-receipt-to") || ci_wait > 0,
        "receipt handoff requires successful CI; --ci-wait must be above zero",
    )?;
    let remote = o.value("remote", "origin");
    require(
        !remote.is_empty()
            && !remote.starts_with('-')
            && remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid remote name",
    )?;
    let target = o.value("target", "main");
    git.run(&["check-ref-format", &format!("refs/heads/{target}")])?;
    let requests_update = requests_target_update(o, ci_wait);
    if requests_update && merge_in_progress(git)? {
        let destination = git.text(&["remote", "get-url", "--push", remote])?;
        require(
            !ci::query_repo(&destination).is_ok_and(|s| s.starts_with("github.com/")),
            UNFINISHED_MERGE,
        )?;
    }
    git.transport_safe()?;
    git.tracked_clean()?;
    let snapshot = git.snapshot()?;
    let local_merge = o.has("local-merge");
    let destination = if local_merge {
        String::new()
    } else {
        git.text(&["remote", "get-url", "--push", remote])?
    };
    require(
        local_merge || destination.lines().count() == 1,
        "land requires one push destination",
    )?;
    let branch = git.text(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let github = ci::query_repo(&destination).is_ok_and(|s| s.starts_with("github.com/"));
    let mode = if local_merge {
        Mode::LocalMerge
    } else if o.has("pr") {
        Mode::Pr
    } else if o.has("direct") {
        Mode::Direct
    } else if env_mode == "pr" {
        Mode::Pr
    } else if env_mode == "direct" || !github {
        Mode::Direct
    } else {
        Mode::Pr
    };
    require(
        mode == Mode::Pr || !(o.has("head-branch") || o.has("merge") || o.has("why")),
        "--head-branch, --merge and --why apply to pull-request mode only",
    )?;
    require(
        mode != Mode::Pr || !o.has("ci-full"),
        "--ci-full is not supported in pull-request mode",
    )?;
    require(
        mode != Mode::Pr || !o.has("merge") || ci_wait > 0,
        "--merge needs --ci-wait above zero",
    )?;
    require(
        mode != Mode::Pr
            || o.has("merge")
            || !(o.has("handoff-receipt-to") || o.has("sync-shared")),
        "--handoff-receipt-to and --sync-shared need --merge in pull-request mode",
    )?;
    let observes_pr_ci = mode == Mode::Pr && ci_wait > 0;
    let auto_update =
        mode == Mode::Pr && github && requests_update && (branch != target || o.has("head-branch"));
    let local_ref = (auto_update && branch != "HEAD").then(|| format!("refs/heads/{branch}"));
    if let Some(previous) = previous_update {
        require(
            auto_update
                && destination == previous.destination
                && snapshot == previous.snapshot
                && git.text(&["symbolic-ref", "HEAD"])? == previous.local_ref,
            "target update checkout or destination changed before the new gate",
        )?;
    }
    if observes_pr_ci {
        require(
            git.text(&["remote", "get-url", "--push", "--all", remote])? == destination,
            "PR observation requires one exact push destination",
        )?;
    }
    let mut head_branch = String::new();
    let mut from_sha = String::new();
    if mode == Mode::Pr {
        head_branch = if let Some(previous) = previous_update {
            previous.branch.clone()
        } else if o.has("head-branch") {
            o.get("head-branch").to_owned()
        } else if branch != "HEAD" && branch != target {
            branch.clone()
        } else {
            default_head_branch(
                &git.text(&["log", "-1", "--format=%s", &snapshot.head])?,
                &snapshot.head,
            )
        };
        require(
            head_branch != target && !head_branch.starts_with('-'),
            "PR head branch must differ from the target branch",
        )?;
        git.run(&["check-ref-format", &format!("refs/heads/{head_branch}")])?;
    } else if mode == Mode::LocalMerge {
        require(
            !o.has("ci-full") && !o.has("handoff-receipt-to") && !o.has("sync-shared"),
            "--local-merge does not use CI, handoff or shared sync",
        )?;
        require(
            branch == target,
            format!("--local-merge requires the checked-out branch to be {target}"),
        )?;
        from_sha = git
            .commit(o.get("from"))
            .map_err(|_| Error::new(2, "--from does not resolve to a commit"))?;
        require(
            !git.succeeds(&["merge-base", "--is-ancestor", &from_sha, &snapshot.head])?,
            "--from is already contained in HEAD; nothing to merge",
        )?;
    }
    let gate = if let Some(previous) = previous_update {
        previous.gate.clone()
    } else if o.has("gate") {
        o.get("gate").into()
    } else if repo.join("scripts/check.sh").is_file() {
        if repo.join("crates/oms").is_dir() {
            let from = if local_merge {
                snapshot.head.clone()
            } else {
                format!("{remote}/{target}")
            };
            format!(
                "bash scripts/check.sh --affected --ci-parity --changed-from {from} --changed-to HEAD"
            )
        } else {
            String::from("bash scripts/check.sh")
        }
    } else {
        return Err(Error::new(2, "no default gate; pass --gate CMD"));
    };
    crate::safe(gate.as_bytes())?;
    let mut moved_before_gate = None;
    let base = if mode == Mode::LocalMerge {
        String::new()
    } else {
        let base = crate::review::remote_tip(git, &destination, target)?
            .ok_or_else(|| Error::new(2, "remote target is absent"))?;
        if !git.succeeds(&["merge-base", "--is-ancestor", &base, &snapshot.head])? {
            let moved = TargetMoved {
                tip: Some(base.clone()),
                refusal: if mode == Mode::Pr {
                    PR_ANCESTOR_REFUSAL
                } else {
                    "remote target is not an ancestor; rebase before land"
                },
            };
            if !auto_update {
                return Err(moved.refuse(""));
            }
            moved_before_gate = Some(moved);
        }
        base
    };
    let mut motivation = String::new();
    if base != snapshot.head && mode == Mode::Direct && github {
        require(
            trivial_docs(git, &base, &snapshot.head)?,
            format!(
                "direct land to {target} is limited to a docs-only change of at most 2 lines; land without --direct to open a pull request"
            ),
        )?;
    } else if base != snapshot.head && mode == Mode::Pr && moved_before_gate.is_none() {
        motivation = pr_motivation(git, o, &base, &snapshot.head)?;
    }
    // Merge choices stay out of the identity: a later `--merge` resumes the
    // awaiting-merge receipt and reuses its exact-head gate.
    let mut request_parts = vec![destination.trim(), remote, target, &gate];
    if mode != Mode::Direct {
        request_parts.extend([mode.name(), head_branch.as_str(), from_sha.as_str()]);
    }
    let stamp = request_stamp(&snapshot.head, &request_parts)?;
    if let Some(previous) = previous_update {
        require(
            stamp == previous.request,
            "target update successor request changed",
        )?;
    }
    let receipt = directory.join(format!("{stamp}.json"));
    let _active = storage::lock(&directory.join("active"), c.interrupted)?;
    if let Some(previous) = previous_update {
        require(
            git.snapshot()? == snapshot
                && git.text(&["symbolic-ref", "HEAD"])? == previous.local_ref,
            "target update checkout changed before the new gate",
        )?;
    }
    if mode == Mode::Pr {
        // Every observer claims its request while holding active. While we hold
        // active, a successful nonblocking acquisition proves no observer can
        // race our admission. A read-only flock probe cannot establish this.
        let claim = storage::file_lock(&directory.join(pr_observation::owner_name(&stamp)))?
            .try_acquire()
            .map_err(|error| match error {
                oms_lock::Error::Busy => {
                    Error::new(75, "exact request already observing CI; use land wait")
                }
                _ => Error::new(error.exit_code(), error.to_string()),
            })?;
        claim
            .release()
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    }
    let holder = obj([
        ("receipt", format!("{stamp}.json").into()),
        ("sha", snapshot.head.clone().into()),
        ("pid", (std::process::id() as usize).into()),
        (
            "start",
            oms_command_operations::lock::process_start(std::process::id())
                .unwrap_or_default()
                .into(),
        ),
    ]);
    save(&directory.join("active-holder"), &holder, c.interrupted)?;
    let previous = storage::read_external(&receipt, true)?;
    let gate_pending = || {
        obj([
            ("command", gate.clone().into()),
            ("rc", "pending".into()),
            ("scope", "unknown".into()),
        ])
    };
    let mut row = if previous.is_empty() {
        obj([
            ("schema", 1usize.into()),
            ("sha", snapshot.head.clone().into()),
            ("request", stamp.clone().into()),
            ("repo", digest(repo.as_os_str().as_encoded_bytes()).into()),
            ("remote", remote.into()),
            ("target", target.into()),
            ("mode", mode.name().into()),
            ("started_at", now().into()),
            ("state", "running".into()),
            ("gate", gate_pending()),
            ("push", obj([("rc", "pending".into())])),
            ("ci", obj([("conclusion", "pending".into())])),
            (
                "update",
                obj([("rc", "skipped".into()), ("reason", "not-reached".into())]),
            ),
        ])
    } else {
        let row = V::parse(&previous)?;
        require(
            string(&row, "sha") == snapshot.head
                && row.get("gate").get("command").text() == Some(&gate)
                && (!observes_pr_ci
                    || (crate::reconcile::matches_checkout(
                        &row,
                        &snapshot.head,
                        &stamp,
                        &digest(repo.as_os_str().as_encoded_bytes()),
                    ) && row.get("mode").text() == Some("pr")
                        && row.get("remote").text() == Some(remote)
                        && row.get("target").text() == Some(target))),
            "land receipt identity mismatch",
        )?;
        if row.get("state").text() == Some("superseded") {
            return Err(Error::new(
                2,
                format!(
                    "land receipt superseded by request {}",
                    string(&row, "successor_request")
                ),
            ));
        }
        row
    };
    if moved_before_gate.is_some() && receipt_has_completed_or_requested_merge(&row) {
        return Err(Error::new(
            2,
            "existing receipt records a completed or requested merge; target update refused",
        ));
    }
    if let Some(previous) = previous_update {
        row.set("target_update", previous.evidence.clone());
        row.set("gate", gate_pending());
    }
    if moved_before_gate.is_some() {
        row.set("head_branch", head_branch.as_str());
    }
    save(&receipt, &row, c.interrupted)?;
    let ctx = Run {
        c,
        o,
        repo,
        git,
        directory,
        receipt: &receipt,
        stamp: &stamp,
        request_parts: &request_parts,
        snapshot: &snapshot,
        gate: &gate,
        destination: &destination,
        remote,
        target,
        ci_wait,
        head_branch: &head_branch,
        local_ref: local_ref.as_deref(),
        from_sha: &from_sha,
        motivation: &motivation,
        auto_update,
        updates,
    };
    if let Some(moved) = moved_before_gate {
        return update_target(&ctx, &mut row, moved, None, true);
    }
    if let Some(failed) = gate_phase(&ctx, &mut row, previous_update.map(|_| base.as_str()))? {
        return Ok(Pass::Done(failed));
    }
    require(
        git.snapshot()? == snapshot,
        "HEAD, tracked tree or index changed while gate ran; nothing pushed",
    )?;
    git.tracked_clean()?;
    if !o.has("ignore-siblings") {
        c.services
            .wait_siblings(repo, Duration::from_secs(sibling_wait), c.interrupted)?;
    }
    match mode {
        Mode::Pr => return land_pr(&ctx, &mut row, &base, _active),
        Mode::LocalMerge => return land_local(&ctx, &mut row).map(Pass::Done),
        Mode::Direct => {}
    }
    if base != snapshot.head {
        let _guard = c
            .services
            .publication_guard(&c.state(repo)?, c.interrupted)?;
        require(git.snapshot()? == snapshot, "tree moved before push")?;
        require(
            crate::review::remote_tip(git, &destination, target)?.as_deref() == Some(&base),
            "remote target changed before push",
        )?;
        git.transport_safe()?;
        let result = git.output(
            &[
                OsStr::new("push"),
                OsStr::new("--no-verify"),
                OsStr::new(&format!("--force-with-lease=refs/heads/{target}:{base}")),
                OsStr::new(remote),
                OsStr::new(&format!("{}:refs/heads/{target}", snapshot.head)),
            ],
            None,
            &[],
        )?;
        row.set(
            "push",
            obj([
                ("rc", V::Num(result.code.to_string())),
                ("base", base.into()),
                ("moved", 1usize.into()),
            ]),
        );
        save(&receipt, &row, c.interrupted)?;
        if result.code != 0 {
            return finish(
                &receipt,
                &mut row,
                "failed",
                "push failed or outcome unknown; inspect remote before retrying",
                c,
            )
            .map(Pass::Done);
        }
    } else if row.get("push").get("rc") != &0usize.into() {
        row.set(
            "push",
            obj([
                ("rc", 0usize.into()),
                ("base", base.into()),
                ("moved", 0usize.into()),
            ]),
        );
        save(&receipt, &row, c.interrupted)?;
    }
    if row.get("push").get("moved") == &1usize.into()
        && let Some(base) = crate::helpers::recorded_base(&row)?
    {
        let _ = crate::attribution::record_commits(
            &c.state(repo)?,
            repo,
            &format!("{base}..{}", snapshot.head),
            &stamp,
            c.interrupted,
        );
    }
    if ci_wait == 0 {
        row.set("ci", obj([("conclusion", "skipped".into())]));
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "ci-skipped".into())]),
        );
        return finish(
            &receipt,
            &mut row,
            "passed",
            "pushed; CI skipped; nothing installed",
            c,
        )
        .map(Pass::Done);
    }
    let deadline = Instant::now() + Duration::from_secs(ci_wait);
    let mut successful = None;
    loop {
        crate::check(c.interrupted)?;
        let query = ci::latest(c, &destination, &snapshot.head, target, "push");
        match query {
            Ok(Some(run)) => {
                let conclusion = ci::conclusion(&run, &snapshot.head)?;
                row.set(
                    "ci",
                    obj([
                        ("conclusion", conclusion.clone().into()),
                        ("run_id", run.get("databaseId").clone()),
                    ]),
                );
                save(&receipt, &row, c.interrupted)?;
                if conclusion == "success" {
                    successful = Some(run);
                    break;
                }
                if conclusion != "pending" {
                    return finish(
                        &receipt,
                        &mut row,
                        "failed",
                        &format!("pushed; CI {conclusion}"),
                        c,
                    )
                    .map(Pass::Done);
                }
            }
            Ok(None) => {}
            Err(e) => {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    &format!("pushed; CI unknown: {e}"),
                    c,
                )
                .map(Pass::Done);
            }
        }
        if Instant::now() >= deadline
            || !(o.has("wait") || o.has("ci-full") || o.has("handoff-receipt-to"))
        {
            break;
        }
        std::thread::park_timeout(
            Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let Some(run) = successful else {
        return finish(
            &receipt,
            &mut row,
            "failed",
            "pushed; CI pending or missing; retry --wait",
            c,
        )
        .map(Pass::Done);
    };
    let detailed = ci::run_details(c, &destination, run.get("databaseId"), &snapshot.head)?;
    let mut scope = ci::full_scope(&detailed, &snapshot.head).unwrap_or("unknown");
    let local_full = row.get("gate").get("scope").text() == Some("full");
    if scope != "full" && !local_full && !o.has("no-update") {
        let full = ci::latest(c, &destination, &snapshot.head, target, "workflow_dispatch")?;
        if let Some(full) = full
            && ci::conclusion(&full, &snapshot.head)? == "success"
        {
            let proof = ci::run_details(c, &destination, full.get("databaseId"), &snapshot.head)?;
            if ci::full_scope(&proof, &snapshot.head).is_ok_and(|s| s == "full") {
                scope = "full";
                let mut ci = row.get("ci").clone();
                ci.set("full_run_id", full.get("databaseId").clone());
                row.set("ci", ci);
            }
        }
        if scope != "full" && o.has("ci-full") && !row.get("ci").get("dispatch").truth() {
            require(
                crate::review::remote_tip(git, &destination, target)?.as_deref()
                    == Some(&snapshot.head),
                "remote target moved before full CI dispatch",
            )?;
            let mut ci = row.get("ci").clone();
            ci.set("dispatch", "requested");
            row.set("ci", ci);
            save(&receipt, &row, c.interrupted)?;
            let bin = std::env::var_os("OMS_GH_BIN").unwrap_or_else(|| "gh".into());
            let result = crate::git::process(
                repo,
                vec![
                    bin,
                    "workflow".into(),
                    "run".into(),
                    "test.yml".into(),
                    "--repo".into(),
                    ci::query_repo(&destination)?.into(),
                    "--ref".into(),
                    target.into(),
                ],
                None,
                Duration::from_secs(30),
                c.interrupted,
                &[],
            )?;
            if result.code != 0 {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    "full CI dispatch outcome unknown; request will not be repeated",
                    c,
                )
                .map(Pass::Done);
            }
        }
        if scope != "full"
            && (o.has("wait") || o.has("ci-full") || o.has("handoff-receipt-to"))
            && row.get("ci").get("dispatch").truth()
        {
            while Instant::now() < deadline {
                crate::check(c.interrupted)?;
                if let Some(full) =
                    ci::latest(c, &destination, &snapshot.head, target, "workflow_dispatch")?
                {
                    let conclusion = ci::conclusion(&full, &snapshot.head)?;
                    if conclusion == "success" {
                        let details = ci::run_details(
                            c,
                            &destination,
                            full.get("databaseId"),
                            &snapshot.head,
                        )?;
                        if ci::full_scope(&details, &snapshot.head).is_ok_and(|s| s == "full") {
                            scope = "full";
                            let mut ci = row.get("ci").clone();
                            ci.set("full_run_id", full.get("databaseId").clone());
                            row.set("ci", ci);
                            break;
                        }
                    } else if conclusion != "pending" {
                        return finish(
                            &receipt,
                            &mut row,
                            "failed",
                            "full CI did not succeed for the exact SHA",
                            c,
                        )
                        .map(Pass::Done);
                    }
                }
                std::thread::park_timeout(
                    Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            if scope != "full" {
                return finish(
                    &receipt,
                    &mut row,
                    "failed",
                    "full CI remains pending or unproved; retry --wait",
                    c,
                )
                .map(Pass::Done);
            }
        }
    }
    let mut ci_row = row.get("ci").clone();
    ci_row.set("scope", scope);
    row.set("ci", ci_row);
    if o.has("no-update") {
        row.set(
            "update",
            obj([("rc", "skipped".into()), ("reason", "disabled".into())]),
        );
    } else if scope == "full" || local_full {
        require(
            git.snapshot()? == snapshot
                && crate::review::remote_tip(git, &destination, target)?.as_deref()
                    == Some(&snapshot.head),
            "install target moved from verified SHA",
        )?;
        let update = c.services.after_land(repo, &row, c.interrupted)?;
        row.set("update", update);
        if row.get("update").get("rc") != &0usize.into() {
            return finish(
                &receipt,
                &mut row,
                "failed",
                "pushed; CI passed; installation failed or was deferred",
                c,
            )
            .map(Pass::Done);
        }
    } else {
        row.set(
            "update",
            obj([
                ("rc", "skipped".into()),
                ("reason", "full-ci-required".into()),
            ]),
        );
    }
    save(&receipt, &row, c.interrupted)?;
    let mut output = finish(
        &receipt,
        &mut row,
        "passed",
        if scope == "full" || local_full {
            "pushed; exact-SHA CI and full proof passed"
        } else {
            "pushed; CI passed; full proof missing; nothing installed"
        },
        c,
    )?;
    tail(&ctx, &mut output)?;
    Ok(Pass::Done(output))
}
fn tail(r: &Run<'_>, output: &mut Output) -> Result<()> {
    let (c, o, repo, receipt, remote, target) = (r.c, r.o, r.repo, r.receipt, r.remote, r.target);
    if let Some(destination) = o.path("handoff-receipt-to", c.cwd) {
        crate::notes::handoff(repo, &destination, receipt, c.interrupted)?;
        output
            .stdout
            .extend_from_slice(b"land: exact receipt and range boundary handed off\n");
    }
    if let Some(destination) = o.path("sync-shared", c.cwd) {
        let mut sync_options = Options::default();
        sync_options
            .values
            .insert("repo".into(), vec![destination.into_os_string()]);
        sync_options
            .values
            .insert("remote".into(), vec![remote.into()]);
        sync_options
            .values
            .insert("target".into(), vec![target.into()]);
        // The publication already happened, so a deferred or refused sync is
        // reported beside the passed land instead of failing it.
        match sync(c, &sync_options) {
            Ok(synced) => {
                output.stdout.extend(synced.stdout);
                output.stderr.extend(synced.stderr);
            }
            Err(error) if error.code == 130 => return Err(error),
            Err(error) => {
                // Some cancelled steps surface as 75 or 2; the flag decides.
                crate::check(c.interrupted)?;
                let state = if error.code == 75 {
                    "deferred"
                } else {
                    "refused"
                };
                output
                    .stderr
                    .extend(format!("land: sync {state}: {error}\n").bytes());
            }
        }
    }
    Ok(())
}
pub(crate) fn sync(c: &Context<'_>, o: &Options) -> Result<Output> {
    parent()?;
    require(o.words.is_empty(), "unexpected sync-shared argument")?;
    let repo = c.state(&c.repo(o)?)?;
    let git = Git::new(&repo, c.interrupted);
    let remote = o.value("remote", "origin");
    let target = o.value("target", "main");
    require(
        !remote.is_empty()
            && !remote.starts_with('-')
            && remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid remote name",
    )?;
    git.run(&["check-ref-format", &format!("refs/heads/{target}")])?;
    let _guard = c.services.publication_guard(&repo, c.interrupted)?;
    let _sync = storage::lock(&repo.join(".oms/locks/canonical-sync"), c.interrupted)?;
    git.safe()?;
    git.run(&["symbolic-ref", "--quiet", "HEAD"])?;
    // Untracked files from other tools do not block a fast-forward; `merge
    // --ff-only` still refuses one that the target would overwrite.
    let tracked_clean = || {
        require(
            git.run(&["status", "--porcelain=v1", "--untracked-files=no"])?
                .is_empty(),
            "tracked worktree or index changed; sync refused",
        )
    };
    tracked_clean()?;
    let before = git.snapshot()?;
    let reference = format!("refs/remotes/{remote}/{target}");
    git.run(&[
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "--no-tags",
        "--",
        remote,
        &format!("+refs/heads/{target}:{reference}"),
    ])?;
    require(
        git.succeeds(&["merge-base", "--is-ancestor", "HEAD", &reference])?,
        "shared checkout is ahead or diverged; sync refused",
    )?;
    tracked_clean()?;
    require(
        git.snapshot()? == before,
        "shared checkout changed during fetch",
    )?;
    let raw = git.run(&["merge", "--ff-only", &reference])?;
    Ok(Output {
        stdout: raw,
        ..Output::default()
    })
}
fn conflicts(c: &Context<'_>, o: &Options) -> Result<Output> {
    let repo = c.repo(o)?;
    let git = Git::new(&repo, c.interrupted);
    if git.safe().is_err() {
        return Ok(Output::text(
            "land conflicts: not checked (unsafe Git config)\n",
        ));
    }
    let max = o
        .value("max", "12")
        .parse::<usize>()
        .map_err(|_| Error::new(2, "--max must be 1..24"))?;
    require((1..=24).contains(&max), "--max must be 1..24")?;
    let base = git.commit(o.value("base", "main"))?;
    let branches = if o.words.len() > 1 {
        o.words[1..].to_vec()
    } else {
        git.text(&[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/oms/",
        ])?
        .lines()
        .map(str::to_owned)
        .collect()
    };
    let branches: Vec<_> = branches.into_iter().take(max).collect();
    let mut rows = vec![];
    for (i, left) in std::iter::once(&base).chain(branches.iter()).enumerate() {
        for right in branches.iter().skip(i) {
            let out = git.output(
                &[
                    "merge-tree".as_ref(),
                    "--write-tree".as_ref(),
                    left.as_ref(),
                    right.as_ref(),
                ],
                None,
                &[],
            )?;
            if out.code == 129 {
                return Ok(Output::text(
                    "land conflicts: Git too old (requires 2.38+)\n",
                ));
            }
            rows.push(obj([
                ("left", left.clone().into()),
                ("right", right.clone().into()),
                ("clean", (out.code == 0).into()),
                (
                    "detail",
                    String::from_utf8_lossy(&out.stdout).into_owned().into(),
                ),
            ]));
        }
    }
    if o.has("json") {
        Output::json(&obj([("limit", max.into()), ("pairs", V::Arr(rows))]))
    } else {
        let mut out = format!("land conflicts: at most {max} branches\n");
        for row in rows {
            out += &format!(
                "{} + {}: {}\n",
                string(&row, "left"),
                string(&row, "right"),
                if row.get("clean").truth() {
                    "clean"
                } else {
                    "conflicts"
                }
            );
        }
        out += "landing order suggestion: ";
        out += &branches.join(" -> ");
        out.push('\n');
        Ok(Output::text(out))
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn receipt_merge_guard_allows_only_unrequested_merge() {
        let cases = [
            (r#"{"state":"running"}"#, false),
            (
                r#"{"state":"awaiting-merge","merge":{"state":"not-requested"}}"#,
                false,
            ),
            (
                r#"{"state":"passed","merge":{"state":"not-requested"}}"#,
                true,
            ),
            (r#"{"state":"running","merge":{"state":"requested"}}"#, true),
            (r#"{"state":"running","merge":{}}"#, true),
            (r#"{"state":"running","merge":null}"#, true),
        ];
        for (json, expected) in cases {
            let row = V::parse(json.as_bytes()).unwrap();
            assert_eq!(
                receipt_has_completed_or_requested_merge(&row),
                expected,
                "{json}"
            );
        }
    }

    #[test]
    fn conflicted_target_update_restores_snapshot() {
        let path = std::env::temp_dir().join(format!("oms-rollback-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&path)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(path.join("file"), "base\n").unwrap();
        run(&["add", "file"]);
        run(&["commit", "-qm", "base"]);
        run(&["checkout", "-qb", "target"]);
        std::fs::write(path.join("file"), "target\n").unwrap();
        run(&["commit", "-qam", "target"]);
        let interrupted = AtomicBool::new(false);
        let git = Git::new(&path, &interrupted);
        let tip = git.commit("HEAD").unwrap();
        run(&["checkout", "-qb", "feature", "HEAD^"]);
        std::fs::write(path.join("file"), "feature\n").unwrap();
        run(&["commit", "-qam", "feature"]);
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(path.join("file"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        std::fs::File::open(path.join("file"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        let before = git.snapshot().unwrap();
        assert_eq!(
            before.index_sha256,
            digest(&std::fs::read(path.join(".git/index")).unwrap())
        );
        let tracked_before = tracked_modes(&git).unwrap();
        let local_ref = git.text(&["symbolic-ref", "HEAD"]).unwrap();
        let index = path.join(git.text(&["rev-parse", "--git-path", "index"]).unwrap());
        let index_bytes = storage::read_external(&index, false).unwrap();
        assert_eq!(digest(&index_bytes), before.index_sha256);
        assert!(
            !Command::new("git")
                .current_dir(&path)
                .args(["merge", "--no-edit", "--no-ff", &tip])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(git.commit("MERGE_HEAD").unwrap(), tip);
        rollback_target_update(
            &git,
            &before,
            &local_ref,
            &tip,
            &index,
            &index_bytes,
            &tracked_before,
        )
        .unwrap();
        assert_eq!(storage::read_external(&index, false).unwrap(), index_bytes);
        assert!(!merge_in_progress(&git).unwrap());
        assert_eq!(git.text(&["symbolic-ref", "HEAD"]).unwrap(), local_ref);
        assert_eq!(git.commit("HEAD").unwrap(), before.head);
        assert_eq!(git.text(&["write-tree"]).unwrap(), before.tree);
        assert_eq!(std::fs::read(path.join("file")).unwrap(), b"feature\n");
        assert_eq!(
            std::fs::metadata(path.join("file")).unwrap().mode() & 0o777,
            0o644
        );
        let mut after = git.snapshot().unwrap();
        // The restored index bytes can contain pre-merge stat entries; a later
        // snapshot may refresh them for the file recreated by merge --abort.
        after.index_sha256 = before.index_sha256.clone();
        assert_eq!(after, before);
    }
}
