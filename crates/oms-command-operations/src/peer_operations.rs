//! Bounded read-only reader for `.oms/artifacts/mcp/<operation>/` peer records.
//! Reuses the same record layout and running/stalled/done + age rules as `oms_peer_operations`.
use crate::value::V;
use chrono::{DateTime, Utc};
use oms_safe_fs::{Policy, Root};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags};
use rustix::process::{Pid, getuid, test_kill_process};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RUN_ROOT: &str = ".oms/artifacts/mcp";
pub const META_LIMIT: usize = 256 * 1024;
pub const STATUS_LIMIT: usize = 4096;
pub const MAX_SCAN_ENTRIES: usize = 4096;
const MAX_OPEN_CANDIDATES: usize = 64;
pub const MAX_OPERATIONS: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerOperationRecord {
    pub operation: String,
    pub kind: String,
    pub status: &'static str,
    pub cancelled: bool,
    pub exit: Option<i64>,
    pub started_at: String,
    pub updated_at: String,
    pub observed_updated_at: Option<String>,
    /// When the owner published its exit status; `None` unless the run is done.
    pub finished_at: Option<String>,
    pub age_seconds: Option<i64>,
    pub targets: Vec<String>,
    pub worktree: Option<String>,
    pub title: Option<String>,
    pub caller: Option<String>,
    pub attempt_id: Option<String>,
}

pub fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub fn is_mcp_operation_id(s: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\A[a-z]+-[0-9TZ]+-[0-9a-f]+\z").expect("fixed regex"))
        .is_match(s)
}

pub fn is_peer_operation_id(s: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\A[a-z]+(?:-[a-z]+)?-[0-9TZ]+-[0-9a-f]+\z").expect("fixed regex")
    })
    .is_match(s)
}

fn format_compact_stamp(s: &str) -> String {
    if s.is_ascii() && s.len() == 16 && s.as_bytes()[8] == b'T' {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &s[..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15]
        )
    } else {
        String::new()
    }
}

pub fn started(id: &str) -> (&str, String) {
    let tail: Vec<&str> = id.rsplitn(3, '-').collect();
    if tail.len() == 3 {
        let stamp = format_compact_stamp(tail[1]);
        if !stamp.is_empty() {
            return (tail[2], stamp);
        }
    }
    let mut parts = id.split('-');
    let kind = parts.next().unwrap_or("");
    let raw = parts.next().unwrap_or("");
    (kind, format_compact_stamp(raw))
}

fn sort_stamp(id: &str) -> &str {
    let tail: Vec<&str> = id.rsplitn(3, '-').collect();
    if tail.len() == 3 && tail[1].is_ascii() && tail[1].len() == 16 && tail[1].as_bytes()[8] == b'T'
    {
        tail[1]
    } else {
        id.split('-').nth(1).unwrap_or("")
    }
}

pub fn sort_operation_names(entries: &mut [String]) {
    entries.sort_by(|a, b| sort_stamp(b).cmp(sort_stamp(a)).then_with(|| b.cmp(a)));
}

fn read_repo_file_owned(
    repo: &Path,
    label: &str,
    limit: usize,
    owner: u32,
) -> std::result::Result<Vec<u8>, String> {
    Root::open(repo)
        .and_then(|r| {
            r.read(
                label,
                limit,
                Policy {
                    single_link: true,
                    owner: Some(owner),
                    max_components: Some(64),
                },
            )
        })
        .map_err(|e| {
            format!("error: missing, unsafe, changed, or oversized operation file ({e:?})")
        })
}

pub fn read_repo_file(
    repo: &Path,
    label: &str,
    limit: usize,
) -> std::result::Result<Vec<u8>, String> {
    read_repo_file_owned(repo, label, limit, getuid().as_raw())
}

fn read_run_owned(
    dir: &Path,
    name: &str,
    limit: usize,
    owner: u32,
) -> std::result::Result<Vec<u8>, String> {
    let repo = dir
        .ancestors()
        .nth(4)
        .ok_or("error: invalid operation directory")?;
    let op = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("error: invalid operation")?;
    read_repo_file_owned(repo, &format!("{RUN_ROOT}/{op}/{name}"), limit, owner)
}

pub fn read_run(dir: &Path, name: &str, limit: usize) -> std::result::Result<Vec<u8>, String> {
    read_run_owned(dir, name, limit, getuid().as_raw())
}

pub fn status(dir: &Path) -> (&'static str, Option<i64>) {
    status_for_owner(dir, getuid().as_raw())
}

fn status_for_owner(dir: &Path, owner: u32) -> (&'static str, Option<i64>) {
    status_with_owner(
        dir,
        owner,
        || read_run_owned(dir, "status", STATUS_LIMIT, owner),
        |pid| test_kill_process(pid) == Err(rustix::io::Errno::SRCH),
    )
}

pub fn status_with(
    dir: &Path,
    read_status: impl Fn() -> std::result::Result<Vec<u8>, String>,
    owner_gone: impl FnOnce(Pid) -> bool,
) -> (&'static str, Option<i64>) {
    status_with_owner(dir, getuid().as_raw(), read_status, owner_gone)
}

fn status_with_owner(
    dir: &Path,
    owner: u32,
    read_status: impl Fn() -> std::result::Result<Vec<u8>, String>,
    owner_gone: impl FnOnce(Pid) -> bool,
) -> (&'static str, Option<i64>) {
    let terminal_exit = || {
        let read = match read_status() {
            // The owner can publish the status between the secure read and
            // this presence check. Confirm the bytes before treating it as invalid.
            Err(_)
                if dir
                    .join("status")
                    .symlink_metadata()
                    .is_ok_and(|m| m.uid() == owner) =>
            {
                read_status()
            }
            Err(_) => return None,
            result => result,
        };
        Some(read.map_or(1, |bytes| {
            String::from_utf8_lossy(&bytes)
                .trim_matches(whitespace)
                .parse()
                .unwrap_or(1)
        }))
    };
    if let Some(exit) = terminal_exit() {
        return ("done", Some(exit));
    }
    let pid = read_run_owned(dir, "pid", STATUS_LIMIT, owner)
        .ok()
        .and_then(|b| {
            String::from_utf8_lossy(&b)
                .trim_matches(whitespace)
                .parse::<i32>()
                .ok()
        })
        .filter(|pid| *pid > 0)
        .and_then(Pid::from_raw);
    if pid.is_some_and(owner_gone) {
        // A completed owner publishes its exit before it can disappear.
        return terminal_exit().map_or(("stalled", None), |exit| ("done", Some(exit)));
    }
    ("running", None)
}

fn stat_modified(stat: &rfs::Stat) -> Option<SystemTime> {
    let secs = u64::try_from(stat.st_mtime).ok()?;
    let nsecs = u32::try_from(stat.st_mtime_nsec)
        .unwrap_or(0)
        .min(999_999_999);
    UNIX_EPOCH.checked_add(Duration::new(secs, nsecs))
}

fn regular_entry_modified(dir_fd: impl AsFd, name: &str, uid: u32) -> Option<SystemTime> {
    let stat = rfs::statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile && stat.st_uid == uid)
        .then(|| stat_modified(&stat))
        .flatten()
}

fn open_owned_dir(dir: &Path, uid: u32) -> Option<std::fs::File> {
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let file: std::fs::File = rfs::open(dir, flags, Mode::empty()).ok()?.into();
    let stat = rfs::fstat(&file).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::Directory && stat.st_uid == uid)
        .then_some(file)
}

fn age_in_dir(dir_fd: impl AsFd, uid: u32, now: SystemTime) -> Option<i64> {
    let modified = regular_entry_modified(&dir_fd, "prompt.txt", uid).or_else(|| {
        let stat = rfs::fstat(&dir_fd).ok()?;
        (FileType::from_raw_mode(stat.st_mode) == FileType::Directory && stat.st_uid == uid)
            .then(|| stat_modified(&stat))
            .flatten()
    })?;
    Some(match now.duration_since(modified) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

pub fn age(dir: &Path) -> Option<i64> {
    age_at(dir, SystemTime::now())
}

pub fn age_at(dir: &Path, now: SystemTime) -> Option<i64> {
    let uid = getuid().as_raw();
    let dir_fd = open_owned_dir(dir, uid)?;
    age_in_dir(&dir_fd, uid, now)
}

fn open_verified_dir(repo: &Path, relative: &str) -> Option<std::fs::File> {
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let uid = getuid().as_raw();
    let mut current: std::fs::File = rfs::open(repo, flags, Mode::empty()).ok()?.into();
    let root_meta = current.metadata().ok()?;
    if root_meta.uid() != uid || root_meta.mode() & 0o002 != 0 {
        return None;
    }
    for segment in relative.split('/').filter(|s| !s.is_empty()) {
        current = rfs::openat(&current, segment, flags, Mode::empty())
            .ok()?
            .into();
        let meta = current.metadata().ok()?;
        if meta.uid() != uid || meta.mode() & 0o002 != 0 {
            return None;
        }
    }
    Some(current)
}

fn safe_token(raw: &str, max_len: usize) -> Option<String> {
    let trimmed = raw.trim_matches(whitespace);
    (!trimmed.is_empty()
        && trimmed.len() <= max_len
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains(['/', '\\'])
        && !trimmed.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

fn valid_cancel_observed_owned(dir: &Path, op: &str, owner: u32) -> bool {
    read_run_owned(dir, "cancel-observed.json", STATUS_LIMIT, owner)
        .ok()
        .and_then(|bytes| V::parse_with_depth(&bytes, 16).ok())
        .is_some_and(|v| {
            matches!(v, V::Obj(_))
                && matches!(v.get("schema"), V::Num(n) if n == "1")
                && v.get("task_id").text() == Some(op)
                && v.get("kind").text() == Some("mcp-task-cancel-observed")
        })
}

fn observed_updated_in_dir(dir_fd: impl AsFd, uid: u32) -> Option<String> {
    ["status", "cancel-request.json", "run.log"]
        .iter()
        .filter_map(|name| regular_entry_modified(&dir_fd, name, uid))
        .max()
        .map(|t| {
            DateTime::<Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
}

pub fn updated_stamp(dir: &Path, fallback: &str) -> String {
    let uid = getuid().as_raw();
    open_owned_dir(dir, uid)
        .and_then(|fd| observed_updated_in_dir(&fd, uid))
        .unwrap_or_else(|| fallback.to_owned())
}

pub fn list_peer_operations(
    repo: &Path,
    limit: usize,
    now: SystemTime,
) -> Vec<PeerOperationRecord> {
    let cap = limit.min(MAX_OPERATIONS);
    if cap == 0 {
        return Vec::new();
    }
    let Some(run_root_fd) = open_verified_dir(repo, RUN_ROOT) else {
        return Vec::new();
    };
    let Ok(read_dir) = std::fs::read_dir(repo.join(RUN_ROOT)) else {
        return Vec::new();
    };
    let uid = getuid().as_raw();
    let dir_flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut names = Vec::new();
    for entry in read_dir.take(MAX_SCAN_ENTRIES).flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_peer_operation_id(&name) && name.len() <= 160 {
            names.push(name);
        }
    }
    sort_operation_names(&mut names);
    let mut records = Vec::new();
    for name in names.into_iter().take(MAX_OPEN_CANDIDATES) {
        let Ok(sub_fd) = rfs::openat(&run_root_fd, name.as_str(), dir_flags, Mode::empty()) else {
            continue;
        };
        let sub_file = std::fs::File::from(sub_fd);
        if !sub_file
            .metadata()
            .is_ok_and(|m| m.uid() == uid && m.mode() & 0o002 == 0)
        {
            continue;
        }
        let dir = repo.join(RUN_ROOT).join(&name);
        let meta = read_run(&dir, "meta.json", META_LIMIT)
            .ok()
            .and_then(|bytes| V::parse_with_depth(&bytes, 32).ok())
            .filter(|v| matches!(v, V::Obj(_)))
            .unwrap_or_else(|| crate::value::obj([]));
        let (fallback_kind, fallback_started) = started(&name);
        let kind = meta
            .get("kind")
            .text()
            .and_then(|s| safe_token(s, 64))
            .unwrap_or_else(|| fallback_kind.to_owned());
        let started_at = meta
            .get("started_at")
            .text()
            .map(|s| s.trim_matches(whitespace))
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .map(str::to_owned)
            .unwrap_or(fallback_started);
        let (op_status, exit) = status_for_owner(&dir, uid);
        let cancelled = op_status == "done" && valid_cancel_observed_owned(&dir, &name, uid);
        let observed_updated_at = observed_updated_in_dir(&sub_file, uid);
        let finished_at = (op_status == "done")
            .then(|| regular_entry_modified(&sub_file, "status", uid))
            .flatten()
            .map(|t| {
                DateTime::<Utc>::from(t)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            });
        let updated_at = observed_updated_at
            .clone()
            .unwrap_or_else(|| started_at.clone());
        let age_seconds = age_in_dir(&sub_file, uid, now);
        let mut targets = meta
            .get("targets")
            .array()
            .iter()
            .filter_map(|item| item.text().and_then(|s| safe_token(s, 200)))
            .take(8)
            .collect::<Vec<_>>();
        if targets.is_empty()
            && let Some(provider) = meta.get("provider").text().and_then(|s| safe_token(s, 64))
        {
            targets.push(provider);
        }
        let worktree = meta
            .get("worktree")
            .text()
            .or_else(|| meta.get("checkout").text())
            .or_else(|| meta.get("refs").get("worktree").text())
            .and_then(|s| safe_token(s, 160));
        let title = meta
            .get("title")
            .text()
            .map(|s| s.trim_matches(whitespace))
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(160).collect::<String>());
        let caller = meta
            .get("caller")
            .text()
            .or_else(|| meta.get("main").text())
            .or_else(|| meta.get("actor").text())
            .and_then(|s| safe_token(s, 96));
        let attempt_id = meta
            .get("attempt_id")
            .text()
            .or_else(|| meta.get("attempt").text())
            .and_then(|s| safe_token(s, 160));
        records.push(PeerOperationRecord {
            operation: name,
            kind,
            status: op_status,
            cancelled,
            exit,
            started_at,
            updated_at,
            observed_updated_at,
            finished_at,
            age_seconds,
            targets,
            worktree,
            title,
            caller,
            attempt_id,
        });
    }
    // Running records outrank newer finished ones, so a live run is never cut by the cap.
    records.sort_by_key(|r| r.status != "running");
    records.truncate(cap);
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("oms-peer-ops-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&path)
                .unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_hyphenated_and_single_word_peer_operation_records() {
        assert_eq!(
            started("consult-20261125T120000Z-aabbccdd"),
            ("consult", "2026-11-25T12:00:00Z".to_owned())
        );
        assert_eq!(
            started("peer-delegate-20261125T120500Z-11223344"),
            ("peer-delegate", "2026-11-25T12:05:00Z".to_owned())
        );

        let repo = TempDir::new("records");
        let run_dir = repo
            .0
            .join(RUN_ROOT)
            .join("peer-delegate-20261125T120500Z-11223344");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        std::fs::write(run_dir.join("prompt.txt"), "Implement board projection\n").unwrap();
        std::fs::write(run_dir.join("pid"), format!("{}\n", std::process::id())).unwrap();
        std::fs::write(
            run_dir.join("meta.json"),
            r#"{"operation":"peer-delegate-20261125T120500Z-11223344","kind":"peer-delegate","targets":["codex:model=gpt-5"],"worktree":"wt-board465","started_at":"2026-11-25T12:05:00Z","title":"Project external Main"}"#,
        )
        .unwrap();

        let rows = list_peer_operations(&repo.0, 20, SystemTime::now());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "peer-delegate");
        assert_eq!(rows[0].status, "running");
        assert!(!rows[0].cancelled);
        assert_eq!(rows[0].worktree.as_deref(), Some("wt-board465"));
        assert_eq!(rows[0].targets, vec!["codex:model=gpt-5".to_owned()]);
        assert!(rows[0].age_seconds.is_some());

        // Over 256 older completed directories must not crowd out a newer running operation
        // before timestamp sorting.
        for idx in 0..270u32 {
            let old_dir = repo
                .0
                .join(RUN_ROOT)
                .join(format!("consult-20260101T000000Z-{idx:08x}"));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&old_dir)
                .unwrap();
            std::fs::write(old_dir.join("status"), "0\n").unwrap();
        }
        let cancelled_id = "peer-review-20261125T120400Z-55667788";
        let cancelled_dir = repo.0.join(RUN_ROOT).join(cancelled_id);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&cancelled_dir)
            .unwrap();
        std::fs::write(cancelled_dir.join("status"), "125\n").unwrap();
        std::fs::write(
            cancelled_dir.join("cancel-observed.json"),
            format!(
                r#"{{"schema":1,"task_id":"{cancelled_id}","kind":"mcp-task-cancel-observed"}}"#
            ),
        )
        .unwrap();

        let many = list_peer_operations(&repo.0, 20, SystemTime::now());
        assert_eq!(many.len(), 20);
        assert_eq!(many[0].operation, "peer-delegate-20261125T120500Z-11223344");
        assert_eq!(many[0].status, "running");
        assert_eq!(many[1].operation, cancelled_id);
        assert_eq!(many[1].status, "done");
        assert!(many[1].cancelled);
        assert!(many[0].finished_at.is_none());
        assert!(many[1].finished_at.is_some());

        // A running operation behind newer finished ones still survives a small cap.
        for idx in 0..4u32 {
            let done = repo
                .0
                .join(RUN_ROOT)
                .join(format!("consult-20261124T00000{idx}Z-{idx:08x}"));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&done)
                .unwrap();
            std::fs::write(done.join("status"), "0\n").unwrap();
        }
        let old_running = repo
            .0
            .join(RUN_ROOT)
            .join("peer-delegate-20261001T000000Z-99887766");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&old_running)
            .unwrap();
        std::fs::write(old_running.join("pid"), format!("{}\n", std::process::id())).unwrap();
        let capped = list_peer_operations(&repo.0, 3, SystemTime::now());
        assert_eq!(capped.len(), 3);
        assert!(capped[..2].iter().all(|r| r.status == "running"));
        assert!(
            capped
                .iter()
                .any(|r| r.operation == "peer-delegate-20261001T000000Z-99887766")
        );
    }

    #[test]
    fn rejects_record_files_owned_by_another_uid() {
        let repo = TempDir::new("owner-uid");
        let op = "consult-20261125T121000Z-abcdef01";
        let run_dir = repo.0.join(RUN_ROOT).join(op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = exited.id();
        let _ = exited.wait();
        std::fs::write(run_dir.join("meta.json"), r#"{"title":"foreign"}"#).unwrap();
        std::fs::write(run_dir.join("pid"), format!("{gone_pid}\n")).unwrap();
        std::fs::write(run_dir.join("status"), "0\n").unwrap();
        std::fs::write(
            run_dir.join("cancel-observed.json"),
            format!(r#"{{"schema":1,"task_id":"{op}","kind":"mcp-task-cancel-observed"}}"#),
        )
        .unwrap();
        std::fs::write(
            run_dir.join("cancel-request.json"),
            format!(r#"{{"schema":1,"task_id":"{op}","kind":"mcp-task-cancel-request"}}"#),
        )
        .unwrap();

        let current_uid = getuid().as_raw();
        let other_uid = current_uid.wrapping_add(1);
        for name in [
            "meta.json",
            "pid",
            "status",
            "cancel-observed.json",
            "cancel-request.json",
        ] {
            assert!(read_run(&run_dir, name, META_LIMIT).is_ok(), "{name}");
            assert!(
                read_run_owned(&run_dir, name, META_LIMIT, other_uid).is_err(),
                "expected foreign owner rejection for {name}"
            );
        }
        assert_eq!(status_for_owner(&run_dir, current_uid), ("done", Some(0)));
        assert_eq!(status_for_owner(&run_dir, other_uid), ("running", None));
        assert!(valid_cancel_observed_owned(&run_dir, op, current_uid));
        assert!(!valid_cancel_observed_owned(&run_dir, op, other_uid));
    }

    #[test]
    fn timestamps_do_not_follow_symlinks_and_ignore_non_regular_entries() {
        use rustix::fs::{Timespec, Timestamps, utimensat};
        use std::os::unix::fs::symlink;

        let repo = TempDir::new("nofollow-stamps");
        let outside = repo.0.join("outside.txt");
        std::fs::write(&outside, "outside\n").unwrap();
        let old_epoch = 1_700_000_000i64;
        let old_ts = Timespec {
            tv_sec: old_epoch as _,
            tv_nsec: 0,
        };
        utimensat(
            rfs::CWD,
            &outside,
            &Timestamps {
                last_access: old_ts,
                last_modification: old_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();

        let op = "consult-20261125T121500Z-12345678";
        let run_dir = repo.0.join(RUN_ROOT).join(op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        symlink(&outside, run_dir.join("prompt.txt")).unwrap();
        symlink(&outside, run_dir.join("cancel-request.json")).unwrap();
        symlink(&outside, run_dir.join("run.log")).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(run_dir.join("status"))
            .unwrap();

        let now = SystemTime::now();
        let computed_age = age_at(&run_dir, now).expect("directory mtime fallback");
        assert!(
            computed_age < 60,
            "prompt.txt symlink target mtime must not be followed: {computed_age}"
        );
        std::fs::remove_file(run_dir.join("prompt.txt")).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(run_dir.join("prompt.txt"))
            .unwrap();
        utimensat(
            rfs::CWD,
            run_dir.join("prompt.txt"),
            &Timestamps {
                last_access: old_ts,
                last_modification: old_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
        assert!(
            age_at(&run_dir, now).unwrap() < 60,
            "non-regular prompt.txt directory entry must be ignored"
        );

        let fallback = "2026-11-25T12:15:00Z";
        assert_eq!(updated_stamp(&run_dir, fallback), fallback);
        let rows = list_peer_operations(&repo.0, 20, now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].observed_updated_at, None);
        assert_eq!(rows[0].updated_at, fallback);

        std::fs::remove_file(run_dir.join("run.log")).unwrap();
        std::fs::write(run_dir.join("run.log"), "regular log\n").unwrap();
        let reg_epoch = 1_764_073_000i64;
        let reg_ts = Timespec {
            tv_sec: reg_epoch as _,
            tv_nsec: 0,
        };
        utimensat(
            rfs::CWD,
            run_dir.join("run.log"),
            &Timestamps {
                last_access: reg_ts,
                last_modification: reg_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let expected_iso = DateTime::<Utc>::from_timestamp(reg_epoch, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(updated_stamp(&run_dir, fallback), expected_iso);
    }
}
