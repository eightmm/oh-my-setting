//! Retention decisions use durable references and canonical writer locks.
use crate::json::{number, object, string};
use crate::{
    Context, GcHookResult, HostServices, Options, Output, Result, Value, epoch, error, failures,
    helpers, json, json_output, parse_timestamp, require, storage, success, timestamp,
};
use oms_command_telemetry::store as telemetry_store;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
pub(crate) const HELP: &str = "Usage: oms gc [--repo PATH] [--days N] [--dry-run|--apply] [--delete-orphan-files]\nDefault is a read-only retention plan. Live, unknown or referenced evidence is retained.\n";
fn age(path: &Path, cutoff: i64) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| !m.is_symlink())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|t| (t.as_secs() as i64) < cutoff)
}
/// Sorted, bounded, interrupt-checked listing of `dir` entries whose name
/// starts with `prefix`; a missing directory is empty, a symlink or any other
/// open/read failure is an error. Each entry reports whether it is a regular
/// file; symlinks are skipped.
fn scan(ctx: &Context<'_>, dir: &Path, prefix: &str) -> Result<Vec<(std::path::PathBuf, bool)>> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) => require(
            m.is_dir() && !m.is_symlink(),
            "scan root must be a non-symlink directory",
        )?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(error(2, e)),
    }
    let mut found = Vec::new();
    for (seen, entry) in std::fs::read_dir(dir).map_err(|e| error(2, e))?.enumerate() {
        crate::checkpoint(ctx.interrupted)?;
        require(seen < 50000, "state scan entry limit exceeded")?;
        let entry = entry.map_err(|e| error(2, e))?;
        if !entry.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| error(2, e))?;
        if !kind.is_symlink() && (kind.is_file() || kind.is_dir()) {
            found.push((entry.path(), kind.is_file()));
        }
    }
    found.sort();
    Ok(found)
}
/// A `peer-*` wrapper holding only a regular `owner.json`, no checkout, no git
/// registration, and older than `cutoff`. Never follows symlinks.
fn peer_orphan(dir: &Path, cutoff: i64) -> bool {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let Some(repo) = dir.parent().and_then(Path::parent).and_then(Path::parent) else {
        return false;
    };
    if !real_dir(&repo.join(".oms")) || !real_dir(dir.parent().unwrap_or(dir)) || !real_dir(dir) {
        return false;
    }
    let owner = dir.join("owner.json");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return false };
        names.push(entry.file_name());
    }
    if names.len() != 1
        || names[0] != "owner.json"
        || !std::fs::symlink_metadata(&owner).is_ok_and(|m| m.is_file())
        || !age(dir, cutoff)
        || !age(&owner, cutoff)
    {
        return false;
    }
    registry_clear(&repo.join(".git"), &dir.join("tree"))
}
/// Resolve symlink/alias components on the deepest existing prefix while
/// rejecting `.` or `..` segments or dangling symlinks.
fn resolve_physical(path: &Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    let mut existing = std::path::PathBuf::from("/");
    let mut tail = Vec::new();
    let mut missing = false;
    for comp in path.components() {
        match comp {
            Component::RootDir => {}
            Component::Normal(part) if !missing => {
                let next = existing.join(part);
                match std::fs::symlink_metadata(&next) {
                    Ok(_) => existing = next,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        missing = true;
                        tail.push(part);
                    }
                    Err(_) => return None,
                }
            }
            Component::Normal(part) => tail.push(part),
            _ => return None,
        }
    }
    let mut resolved = std::fs::canonicalize(&existing).ok()?;
    for part in tail {
        resolved.push(part);
    }
    Some(resolved)
}

/// True only when git's worktree registry provably does not reference `tree`.
/// Any symlink, unreadable or non-regular entry, non-UTF-8 text, `.`/`..`
/// segment, unresolved alias or relative `gitdir` is unknown, hence not clear.
fn registry_clear(git: &Path, tree: &Path) -> bool {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat};
    use rustix::io::Errno;
    use std::io::Read;
    let Some(canon_tree) = resolve_physical(tree) else {
        return false;
    };
    let git_fd = match open(
        git,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return true,
        Err(_) => return false,
    };
    let registry_fd = match openat(
        &git_fd,
        "worktrees",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return true,
        Err(_) => return false,
    };
    let Ok(entries) = Dir::read_from(&registry_fd) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name();
        if [b".".as_slice(), b"..".as_slice()].contains(&name.to_bytes()) {
            continue;
        }
        let Ok(entry_stat) = statat(&registry_fd, name, AtFlags::SYMLINK_NOFOLLOW) else {
            return false;
        };
        if FileType::from_raw_mode(entry_stat.st_mode) != FileType::Directory {
            return false;
        }
        let Ok(entry_fd) = openat(
            &registry_fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return false;
        };
        let Ok(opened) = fstat(&entry_fd) else {
            return false;
        };
        if opened.st_dev != entry_stat.st_dev || opened.st_ino != entry_stat.st_ino {
            return false;
        }
        let Ok(gitdir_stat) = statat(&entry_fd, c"gitdir", AtFlags::SYMLINK_NOFOLLOW) else {
            return false;
        };
        if FileType::from_raw_mode(gitdir_stat.st_mode) != FileType::RegularFile
            || gitdir_stat.st_size < 0
            || gitdir_stat.st_size > 64 * 1024
        {
            return false;
        }
        let Ok(gitdir_fd) = openat(
            &entry_fd,
            c"gitdir",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return false;
        };
        let mut text = Vec::new();
        if std::fs::File::from(gitdir_fd)
            .take(64 * 1024 + 1)
            .read_to_end(&mut text)
            .is_err()
            || text.len() > 64 * 1024
        {
            return false;
        }
        let Ok(text) = String::from_utf8(text) else {
            return false;
        };
        let target = Path::new(text.trim());
        let Some(canon_target) = resolve_physical(target) else {
            return false;
        };
        if target.starts_with(tree) || canon_target.starts_with(&canon_tree) {
            return false;
        }
    }
    true
}

/// Remove an orphaned `.oms/tmp/peer-<id>` wrapper holding solely a regular
/// `owner.json` using descriptor-anchored `O_NOFOLLOW` handles.
fn remove_peer_wrapper(repo: &Path, dir: &Path) -> Result<bool> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};
    use rustix::io::Errno;
    let Some(id) = dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.starts_with("peer-"))
    else {
        return Ok(false);
    };
    if dir != repo.join(".oms/tmp").join(id) {
        return Ok(false);
    }
    let mut cur = match open(
        repo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
        Err(e) => return Err(error(2, e)),
    };
    for part in [".oms", "tmp"] {
        cur = match openat(
            &cur,
            part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
            Err(e) => return Err(error(2, e)),
        };
    }
    let before = match statat(&cur, id, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => s,
        Err(Errno::NOENT) => return Ok(false),
        Err(e) => return Err(error(2, e)),
    };
    if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
        return Ok(false);
    }
    let wrapper_fd = match openat(
        &cur,
        id,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
        Err(e) => return Err(error(2, e)),
    };
    let opened = fstat(&wrapper_fd).map_err(|e| error(2, e))?;
    if opened.st_dev != before.st_dev || opened.st_ino != before.st_ino {
        return Ok(false);
    }
    let mut names = Vec::new();
    for entry in Dir::read_from(&wrapper_fd).map_err(|e| error(2, e))? {
        let entry = entry.map_err(|e| error(2, e))?;
        let name = entry.file_name();
        if [b".".as_slice(), b"..".as_slice()].contains(&name.to_bytes()) {
            continue;
        }
        names.push(name.to_owned());
    }
    if names.len() != 1 || names[0].as_bytes() != b"owner.json" {
        return Ok(false);
    }
    let owner_stat = match statat(&wrapper_fd, c"owner.json", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => s,
        Err(Errno::NOENT) => return Ok(false),
        Err(e) => return Err(error(2, e)),
    };
    if FileType::from_raw_mode(owner_stat.st_mode) != FileType::RegularFile {
        return Ok(false);
    }
    match unlinkat(&wrapper_fd, c"owner.json", AtFlags::empty()) {
        Ok(()) => {}
        Err(Errno::NOENT) => return Ok(false),
        Err(e) => return Err(error(2, e)),
    }
    drop(wrapper_fd);
    match unlinkat(&cur, id, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(e) => Err(error(2, e)),
    }
}
fn document(ctx: &Context<'_>, path: &Path) -> Result<Value> {
    json::parse(&storage::read(&ctx.repo, path, storage::MAX_FILE, false)?)
}
fn compact(
    ctx: &Context<'_>,
    path: &Path,
    keep: impl Fn(&Value) -> bool,
    apply: bool,
) -> Result<usize> {
    let _guard = storage::lock(&ctx.repo.join(path))?;
    let raw = storage::read(&ctx.repo, path, storage::MAX_STREAM, true)?;
    let mut output = Vec::new();
    let mut removed = 0;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let retain = match json::parse_legacy(line.trim_ascii(), storage::MAX_FILE) {
            Ok(row) => keep(&row),
            Err(_) => true,
        };
        if retain {
            output.extend_from_slice(line);
        } else {
            removed += 1;
        }
    }
    if apply && removed > 0 {
        storage::replace_locked(&ctx.repo, path, &output)?;
    }

    Ok(removed)
}
fn candidate(
    ctx: &Context<'_>,
    items: &mut Vec<Value>,
    kind: &str,
    path: &Path,
    apply: bool,
) -> Result<()> {
    let before = std::fs::symlink_metadata(path).map_err(|e| error(2, e))?;
    require(!before.is_symlink(), "GC refuses a symlink candidate")?;
    if before.is_dir() {
        let mut pending = vec![path.to_owned()];
        let mut count = 0;
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).map_err(|e| error(2, e))? {
                let entry = entry.map_err(|e| error(2, e))?;
                count += 1;
                require(count <= 50000, "GC directory entry limit exceeded")?;
                let kind = entry.file_type().map_err(|e| error(2, e))?;
                require(
                    kind.is_dir() || kind.is_file(),
                    "GC candidate contains unknown or symlinked evidence; retained",
                )?;
                if kind.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
    }
    if apply {
        storage::remove(&ctx.repo, path, &before)?;
    }
    items.push(object([
        ("kind", string(kind)),
        (
            "path",
            string(
                path.strip_prefix(&ctx.repo)
                    .unwrap_or(path)
                    .to_string_lossy(),
            ),
        ),
    ]));
    Ok(())
}
fn plan(
    ctx: &Context<'_>,
    req: &oms_command_plan::Request,
    host: &dyn HostServices,
) -> Result<oms_command_plan::Response> {
    let mut context = oms_command_plan::Context::from_environment(req, &ctx.repo)?;
    context.state_repo = ctx.repo.clone();
    context.plan_file = ctx.repo.join(".oms/plan/tasks.json");
    oms_command_plan::Plan::open(context, host.plan_services(), ctx.interrupted)?.execute(req)
}
fn has_panel_call_artifacts(repo: &Path) -> Result<bool> {
    let entries = match std::fs::read_dir(repo.join(".oms/artifacts")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(error(2, e)),
    };
    for entry in entries {
        let name = entry.map_err(|e| error(2, e))?.file_name();
        if let Some(s) = name.to_str()
            && s.starts_with("panel-call-")
            && s.ends_with(".json")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn run(args: &[OsString], ctx: &Context<'_>, host: &dyn HostServices) -> Result<Output> {
    let opts = Options::parse(
        args,
        &["repo", "days"],
        &["dry-run", "apply", "delete-orphan-files", "json"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(
        opts.pos.is_empty() && !(opts.flag("apply") && opts.flag("dry-run")),
        "choose --dry-run or --apply",
    )?;
    let repo = opts.repo(ctx)?;
    let ctx = Context {
        cwd: repo.clone(),
        repo: repo.clone(),
        state_repo: repo,
        runtime: ctx.runtime,
        interrupted: ctx.interrupted,
    };
    let days = opts.number("days", 30)?;
    let seconds = i64::try_from(days)
        .ok()
        .and_then(|n| n.checked_mul(86400))
        .ok_or_else(|| error(2, "retention days out of range"))?;
    let cutoff = epoch().saturating_sub(seconds);
    let apply = opts.flag("apply");
    let mut items = Vec::new();
    let mut deferred = Vec::new();
    if !ctx.repo.join(".oms").exists() {
        return Ok(success("gc: nothing to reclaim\n"));
    }

    let completion_dirs = [
        ".oms/plan/completion-captures",
        ".oms/plan/completion-prefixes",
        ".oms/plan/completion-segments",
        ".oms/plan/completion-pins",
        ".oms/plan/completions",
        ".oms/plan/completion-pin-leases",
    ];
    if completion_dirs.iter().any(|d| ctx.repo.join(d).is_dir()) {
        if let (Some(_g1), Some(_g2), Some(_g3)) = (
            storage::try_lock(&ctx.repo.join(".oms/landings.jsonl"))?,
            storage::try_lock(&ctx.repo.join(".oms/delegations/.marker-set-lock-target"))?,
            storage::try_lock(&ctx.repo.join(".oms/plan/tasks.json"))?,
        ) {
            let entries = oms_command_plan::completion::gc_completion_captures(
                &ctx.repo,
                cutoff,
                apply,
                host.group_resolver()
                    .or_else(|| host.plan_services().group_membership_resolver()),
            )
            .map_err(|e| error(e.code, e.message))?;
            for (kind, path) in entries {
                items.push(object([("kind", string(kind)), ("path", string(path))]));
            }
        } else {
            deferred.push(string(
                "completion-provenance: skipped while a writer holds the file",
            ));
        }
    }

    if ctx.repo.join(".oms/landings.jsonl").exists()
        || ctx.repo.join(".oms/landing-patches").is_dir()
    {
        match host.gc_landing_captures(&ctx.repo, days as u64, apply)? {
            GcHookResult::Reclaimed(pairs) => {
                for (kind, path) in pairs {
                    items.push(object([("kind", string(kind)), ("path", string(path))]));
                }
            }
            GcHookResult::Deferred(reason) => deferred.push(string(reason)),
        }
    }

    let mut quiescent = true;
    for path in helpers::files(&ctx, &ctx.repo.join(".oms/delegations"), ".json", 50000)? {
        let _guard = storage::lock(&path)?;
        let row = match document(&ctx, &path) {
            Ok(r) => r,
            Err(_) => {
                quiescent = false;
                continue;
            }
        };
        let pid = row
            .get("pid")
            .int()
            .ok()
            .or_else(|| row.text("pid").parse().ok())
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| *n > 0)
            .and_then(rustix::process::Pid::from_raw);
        let dead = pid
            .is_some_and(|p| rustix::process::test_kill_process(p) == Err(rustix::io::Errno::SRCH));
        if !dead {
            quiescent = false;
            continue;
        }
        if !row.text("executor_id").is_empty() {
            deferred.push(string("legacy Soul marker retained"));
            continue;
        }
        if !row.text("task_id").is_empty() {
            let data = storage::read(
                &ctx.repo,
                Path::new(".oms/plan/tasks.json"),
                storage::MAX_FILE,
                true,
            )?;
            if !data.is_empty() {
                let data = json::parse(&data)?;
                if let Some(task) = data.get("tasks").opt(row.text("task_id"))
                    && ["claimed", "running"].contains(&task.text("state"))
                    && !row.text("lease_id").is_empty()
                    && row.text("lease_id") == task.text("lease_id")
                {
                    let mut request =
                        oms_command_plan::Request::new(oms_command_plan::Action::RecoverLease)
                            .with("id", row.text("task_id"))
                            .with("lease-id", row.text("lease_id"))
                            .with("expected-state", task.text("state"))
                            .with("markers-dir", ctx.repo.join(".oms/delegations"));
                    if !apply {
                        request = request.flag("check");
                    }
                    match plan(&ctx, &request, host) {
                        Ok(response) if response.code == 0 => {}
                        _ => {
                            deferred.push(string("orphan task recovery unproven; marker retained"));
                            continue;
                        }
                    }
                }
            }
        }
        let _markers = storage::lock(&ctx.repo.join(".oms/delegations/.marker-set-lock-target"))?;
        if document(&ctx, &path)? == row {
            candidate(&ctx, &mut items, "orphan-delegation", &path, apply)?;
        }
    }
    match crate::archive::compact(&ctx, 0, Some(cutoff), apply) {
        Ok(report) => {
            for (path, family, _, _) in report.entries {
                items.push(object([
                    ("kind", string("archived-cold")),
                    ("family", string(family)),
                    ("path", string(path)),
                ]));
            }
            if report.unlink_failed > 0 {
                deferred.push(string("cold archive originals retained after commit"));
            }
        }
        Err(e) => deferred.push(string(format!("cold archive deferred: {}", e.message))),
    }
    if let Ok(entries) = std::fs::read_dir(ctx.repo.join(".oms/checkpoints")) {
        for entry in entries {
            let entry = entry.map_err(|e| error(2, e))?;
            let path = entry.path();
            if !entry.file_type().map_err(|e| error(2, e))?.is_dir()
                || !entry.file_name().to_string_lossy().starts_with("cp-")
            {
                continue;
            }
            let _guard = storage::lock(&path.join("meta.json"))?;
            if document(&ctx, &path.join("meta.json"))
                .ok()
                .and_then(|v| parse_timestamp(v.text("created_at")))
                .is_some_and(|t| t < cutoff)
            {
                candidate(&ctx, &mut items, "checkpoint", &path, apply)?;
            }
        }
    }
    if quiescent {
        let _markers = storage::lock(&ctx.repo.join(".oms/delegations/.marker-set-lock-target"))?;
        let path = Path::new(".oms/hooks/events.jsonl");
        let n = compact(
            &ctx,
            path,
            |r| parse_timestamp(r.text("ts")).is_none_or(|t| t >= cutoff),
            apply,
        )?;
        if n > 0 {
            items.push(object([
                ("kind", string("hook-events")),
                ("rows", number(n)),
            ]));
        }
        for path in helpers::files(&ctx, &ctx.repo.join(".oms/hooks/sessions"), "", 50000)? {
            if age(&path, cutoff) {
                let Some(_guard) = storage::try_lock_in(Path::new("/"), &path)? else {
                    deferred.push(string(
                        "active hook state lock held; hook retention deferred",
                    ));
                    continue;
                };
                if age(&path, cutoff) {
                    candidate(&ctx, &mut items, "hook-session", &path, apply)?;
                }
            }
        }
        let hooks = ctx.repo.join(".oms/hooks");
        let activity = hooks.join("panel-activity");
        let mut aged = Vec::new();
        for path in helpers::files(&ctx, &activity, ".json", 50000)? {
            if path.parent() == Some(activity.as_path()) {
                aged.push(("hook-panel-activity", path));
            }
        }
        for (path, is_file) in scan(&ctx, &hooks, "state-hint.")? {
            if is_file {
                aged.push(("hook-state-hint", path));
            }
        }
        for (kind, path) in aged {
            if age(&path, cutoff) {
                let Some(_guard) = storage::try_lock_in(Path::new("/"), &path)? else {
                    deferred.push(string(
                        "active hook state lock held; hook retention deferred",
                    ));
                    continue;
                };
                if age(&path, cutoff) {
                    candidate(&ctx, &mut items, kind, &path, apply)?;
                }
            }
        }
        for (dir, is_file) in scan(&ctx, &ctx.repo.join(".oms/tmp"), "peer-")? {
            if is_file || !peer_orphan(&dir, cutoff) {
                continue;
            }
            let Some(_guard) = storage::try_lock_in(&ctx.repo, &dir.join("owner.json"))? else {
                deferred.push(string(
                    "active peer wrapper lock held; orphan reclamation deferred",
                ));
                continue;
            };
            if !peer_orphan(&dir, cutoff) {
                continue;
            }
            if apply && !remove_peer_wrapper(&ctx.repo, &dir)? {
                continue;
            }
            items.push(object([
                ("kind", string("orphan-peer-tmp")),
                (
                    "path",
                    string(
                        dir.strip_prefix(&ctx.repo)
                            .unwrap_or(&dir)
                            .to_string_lossy(),
                    ),
                ),
            ]));
        }
    } else {
        deferred.push(string(
            "hook and dispatch retention deferred while delegation liveness is unknown or live",
        ));
    }
    if ctx.repo.join(".oms/panel-state/dispatch-logs").is_dir()
        || has_panel_call_artifacts(&ctx.repo)?
    {
        match host.gc_dispatch_logs(&ctx.repo, days as u64, apply)? {
            GcHookResult::Reclaimed(pairs) => {
                for (kind, path) in pairs {
                    items.push(object([("kind", string(kind)), ("path", string(path))]));
                }
            }
            GcHookResult::Deferred(reason) => deferred.push(string(reason)),
        }
    }
    let spine = crate::spine::index(&ctx);
    let _spine_guard = storage::lock(&spine)?;
    let (runs, bad) = storage::rows(&ctx.repo, &spine)?;
    require(bad == 0, "malformed spine; run retention refused")?;
    let mut last = BTreeMap::new();
    let mut closed = BTreeSet::new();
    let mut capsule_refs: BTreeMap<String, String> = BTreeMap::new();
    for row in &runs {
        let id = row.text("run_id");
        if id.is_empty() {
            continue;
        }
        if let Some(t) = parse_timestamp(row.text("ts")) {
            let item = last.entry(id.to_owned()).or_insert(t);
            *item = (*item).max(t);
        }
        if row.text("tool") == "oms-run" && row.text("event") == "close" {
            closed.insert(id.to_owned());
        }
        if row.text("tool") == "run-capsule"
            && let Some(name) = Path::new(row.text("path"))
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
        {
            capsule_refs.insert(name.into(), id.into());
        }
    }
    let mut closing = Vec::new();
    for (id, last) in &last {
        if *last < cutoff && !closed.contains(id) {
            items.push(object([
                ("kind", string("stale-run-close")),
                ("run_id", string(id)),
            ]));
            if apply {
                let row = object([
                    ("run_id", string(id)),
                    ("tool", string("oms-run")),
                    ("event", string("close")),
                    ("ts", string(timestamp(epoch())?)),
                    ("detail", string(format!("gc: no event in {days}d"))),
                ]);
                closing.push(row);
                closed.insert(id.clone());
            }
        }
    }
    storage::append_many_locked(&ctx.repo, &spine, &closing)?;
    drop(_spine_guard);
    let _claims = storage::lock(&ctx.repo.join(".oms/claims.jsonl"))?;
    let _figures = storage::lock(&ctx.repo.join(".oms/figures.jsonl"))?;
    let mut references = None;
    for path in helpers::files(
        &ctx,
        &crate::capsule::directory(&ctx),
        "capsule.json",
        50000,
    )? {
        let Some(bundle) = path.parent() else {
            continue;
        };
        let id = bundle.file_name().unwrap_or_default().to_string_lossy();
        let run = capsule_refs.get(id.as_ref());
        if run.is_some_and(|r| !closed.contains(r)) || !age(bundle, cutoff) {
            continue;
        }
        if references.is_none() {
            references = Some([
                storage::rows(&ctx.repo, Path::new(".oms/claims.jsonl"))?,
                storage::rows(&ctx.repo, Path::new(".oms/figures.jsonl"))?,
            ]);
        }
        let pinned = references.as_ref().unwrap().iter().any(|(refs, bad)| {
            *bad > 0
                || refs.iter().any(|r| {
                    r.text("capsule") == id
                        || r.get("capsules")
                            .strings()
                            .unwrap_or_default()
                            .iter()
                            .any(|s| *s == id)
                })
        });
        if !pinned {
            let _guard = storage::lock(&path)?;
            if age(bundle, cutoff) {
                candidate(&ctx, &mut items, "capsule", bundle, apply)?;
            }
        }
    }
    drop(_figures);
    drop(_claims);
    let failures_path = std::env::var_os("OMS_FAIL_LEDGER")
        .map(|p| helpers::absolute(&ctx.cwd, p))
        .unwrap_or(ctx.repo.join(".oms/failures.jsonl"));
    let _guard = storage::lock(&failures_path)?;
    let (rows, bad) = storage::rows(&ctx.repo, &failures_path)?;
    require(
        bad == 0 && rows.iter().all(failures::valid_row),
        "malformed failure ledger; compaction refused",
    )?;
    let projected = failures::project(&rows, 0, "", epoch())?;
    let retired: BTreeSet<_> = projected
        .get("failures")
        .array()?
        .iter()
        .filter(|r| r.get("resolved").truth() || r.get("expired").truth())
        .map(|r| r.text("fingerprint"))
        .collect();
    let retained = rows
        .iter()
        .filter(|r| {
            !retired.contains(r.text("fingerprint"))
                || parse_timestamp(r.text("ts")).is_none_or(|t| t >= cutoff)
        })
        .collect::<Vec<_>>();
    let removed = rows.len() - retained.len();
    if removed > 0 {
        if apply {
            let mut bytes = Vec::new();
            for row in retained {
                bytes.extend(json::encoded(row)?);
            }
            storage::replace_locked(&ctx.repo, &failures_path, &bytes)?;
        }
        items.push(object([
            ("kind", string("failures")),
            ("rows", number(removed)),
        ]));
    }
    drop(_guard);
    let guard = ctx.repo.join(".oms/guards/change-guard.tsv");
    if guard.is_file() {
        let _guard = storage::lock(&guard)?;
        let raw = storage::input(&guard, storage::MAX_FILE)?;
        let text = String::from_utf8_lossy(&raw);
        let mut pid = None;
        let mut started = None;
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('\t') {
                if key == "pid" {
                    pid = value
                        .parse::<i32>()
                        .ok()
                        .and_then(rustix::process::Pid::from_raw);
                } else if key == "started" {
                    started = value.parse::<i64>().ok();
                }
            }
        }
        let stale = if let Some(pid) = pid {
            rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH)
        } else {
            started.is_some_and(|t| t < cutoff) || started.is_none() && age(&guard, cutoff)
        };
        if stale {
            candidate(&ctx, &mut items, "stale-change-guard", &guard, apply)?;
        }
    }
    let usage = ctx.repo.join(".oms/usage.jsonl");
    if usage.is_file() {
        let _guard = storage::lock(&usage)?;
        let (rows, bad) = storage::rows(&ctx.repo, &usage)?;
        if bad == 0 {
            let today = epoch().div_euclid(86400);
            let mut counts: BTreeMap<(String, String), i128> = BTreeMap::new();
            for row in &rows {
                let day = row.text("day");
                let stamp = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|t| t.and_utc().timestamp().div_euclid(86400));
                if stamp.is_none_or(|t| t > today || today - t > 30)
                    || row.text("family").is_empty()
                {
                    continue;
                }
                *counts
                    .entry((row.text("family").into(), day.into()))
                    .or_default() += row.get("count").int().unwrap_or(1);
            }
            if counts.len() < rows.len() {
                let mut bytes = Vec::new();
                for ((family, day), count) in &counts {
                    bytes.extend(json::encoded(&object([
                        ("schema", number(1)),
                        ("family", string(family)),
                        ("day", string(day)),
                        ("count", number(count)),
                    ]))?);
                }
                if apply {
                    storage::replace_locked(&ctx.repo, &usage, &bytes)?;
                }
                items.push(object([
                    ("kind", string("usage")),
                    ("rows", number(rows.len() - counts.len())),
                ]));
            }
        } else {
            deferred.push(string("malformed usage rows retained"));
        }
    }
    for operation in ["reconcile", "compact"] {
        if !ctx.repo.join(".oms/lifecycle/events.jsonl").exists() {
            break;
        }
        let mut args = vec![operation.into()];
        if operation == "compact" {
            args.extend(["--days".into(), days.to_string().into()]);
        } else {
            let stale =
                std::env::var("OMS_ATTEMPT_STALE_SECONDS").unwrap_or_else(|_| "86400".into());
            require(
                stale.parse::<u64>().is_ok(),
                "invalid OMS_ATTEMPT_STALE_SECONDS",
            )?;
            args.extend(["--stale-seconds".into(), stale.into()]);
        }
        if apply {
            args.push("--apply".into());
        }
        let result = oms_command_operations::run(
            oms_command_operations::App::Events,
            &args,
            &ctx.repo,
            ctx.interrupted,
        );
        require(result.0 == 0, String::from_utf8_lossy(&result.2))?;
    }
    for path in helpers::files(&ctx, &ctx.repo.join(".oms/threads"), ".jsonl", 50000)? {
        if !age(&path, cutoff) {
            continue;
        }
        let _guard = storage::lock(&path)?;
        let (rows, bad) = storage::rows(&ctx.repo, &path)?;
        if bad > 0 {
            continue;
        }
        let room = rows.iter().any(|r| r.opt("room_event").is_some());
        let collect = if room {
            let raw = json::canonical(&Value::Array(rows.clone()))?;
            let value = oms_command_room::value::parse(&raw, storage::MAX_STREAM)
                .map_err(|e| error(2, e))?;
            match oms_command_room::project(
                value
                    .as_array()
                    .ok_or_else(|| error(2, "invalid room stream"))?,
            ) {
                Ok(snapshot) => {
                    let snapshot = json::parse(
                        &oms_command_room::value::encode(&snapshot, false, true)
                            .map_err(|e| error(2, e))?,
                    )?;
                    snapshot.get("closed").truth()
                        && snapshot
                            .get("participants")
                            .object()
                            .map(|rows| rows.iter().all(|(_, r)| !r.get("joined").truth()))
                            .unwrap_or(false)
                        && snapshot
                            .get("messages")
                            .array()
                            .map(|rows| {
                                rows.iter().all(|r| {
                                    r.get("pending_for")
                                        .array()
                                        .map(|r| r.is_empty())
                                        .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                }
                Err(_) => false,
            }
        } else {
            rows.iter().any(|r| r.text("role") == "closed")
        };
        if collect && age(&path, cutoff) {
            candidate(
                &ctx,
                &mut items,
                if room { "closed-room" } else { "closed-thread" },
                &path,
                apply,
            )?;
        }
    }
    let (keep, _high) =
        telemetry_store::retention_limits(None, None).map_err(|e| error(e.exit_code(), e))?;
    let body =
        telemetry_store::read_index(&ctx.repo, None, true).map_err(|e| error(e.exit_code(), e))?;
    let source: Vec<&[u8]> = body
        .split_inclusive(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .collect();
    let pinned = telemetry_store::completion_pins(&ctx.repo);
    if !source.is_empty() && pinned.is_none() {
        deferred.push(string(
            "artifact rows retained: completion receipts unavailable",
        ));
    }
    let mut kept_bytes = body.clone();
    let mut pruned = 0usize;
    if !source.is_empty() {
        let options = telemetry_store::RetentionOptions {
            keep,
            pinned_event_ids: pinned,
            ..Default::default()
        };
        let retained = telemetry_store::retained_lines(&source, &options)
            .map_err(|e| error(e.exit_code(), e))?;
        pruned = source.len().saturating_sub(retained.len());
        if pruned > 0 {
            kept_bytes = retained.concat();
            items.push(object([
                ("kind", string("artifact-index")),
                ("rows", number(pruned)),
            ]));
            if apply {
                telemetry_store::replace_bytes(
                    &ctx.repo,
                    None,
                    &kept_bytes,
                    &telemetry_store::AppendOptions {
                        keep: Some(keep as i64),
                        high_water: None,
                        expected: Some(body.clone()),
                    },
                    &[],
                    ctx.interrupted,
                )
                .map_err(|e| error(e.exit_code(), e))?;
            }
        }
    }
    if opts.flag("delete-orphan-files") {
        if telemetry_store::completion_references(&ctx.repo).is_none() {
            deferred.push(string(
                "orphan files retained: completion receipts unavailable",
            ));
        } else if ctx.repo.join(".oms/artifacts").is_dir() {
            let snapshot_body = if apply && pruned > 0 {
                kept_bytes.as_slice()
            } else {
                body.as_slice()
            };
            let report = telemetry_store::delete_orphans(
                &ctx.repo,
                None,
                snapshot_body,
                !apply,
                seconds,
                ctx.interrupted,
            )
            .map_err(|e| error(e.exit_code(), e))?;
            for rel in report.paths {
                items.push(object([
                    ("kind", string("orphan-artifact")),
                    ("path", string(rel)),
                ]));
            }
        }
    }
    let count = items
        .iter()
        .map(|item| {
            item.get("rows")
                .int()
                .ok()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(1)
        })
        .sum::<usize>();
    let report = object([
        ("schema", number(1)),
        ("dry_run", Value::Bool(!apply)),
        ("items", Value::Array(items.clone())),
        ("removed", number(if apply { count } else { 0 })),
        ("deferred", Value::Array(deferred.clone())),
    ]);
    if opts.flag("json") {
        return json_output(0, &report);
    }
    let mut out = String::new();
    for item in items {
        out.push_str(&format!(
            "- {}: {}\n",
            item.text("kind"),
            if !item.text("path").is_empty() {
                item.text("path").to_owned()
            } else if !item.text("run_id").is_empty() {
                item.text("run_id").into()
            } else {
                format!("compact {} rows", helpers::display(item.get("rows")))
            }
        ));
    }
    for item in deferred {
        out.push_str(&format!("- retained: {}\n", item.str()?));
    }
    if count == 0 {
        out.push_str("gc: nothing to reclaim\n");
    } else if apply {
        out.push_str(&format!("gc: {count} item(s) reclaimed\n"));
    } else {
        out.push_str(&format!(
            "gc: {count} item(s) would be reclaimed\ngc: re-run with --apply to remove\n"
        ));
    }
    Ok(success(out))
}
