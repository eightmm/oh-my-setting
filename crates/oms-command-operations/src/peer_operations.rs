//! Bounded read-only reader for `.oms/artifacts/mcp/<operation>/` peer records.
//! Reuses the same record layout and running/stalled/done + age rules as `oms_peer_operations`.
use crate::value::V;
use chrono::{DateTime, Utc};
use oms_safe_fs::{Policy, Root};
use rustix::fs::{self as rfs, Mode, OFlags};
use rustix::process::{Pid, getuid, test_kill_process};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

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

pub fn read_repo_file(
    repo: &Path,
    label: &str,
    limit: usize,
) -> std::result::Result<Vec<u8>, String> {
    Root::open(repo)
        .and_then(|r| {
            r.read(
                label,
                limit,
                Policy {
                    single_link: true,
                    owner: None,
                    max_components: Some(64),
                },
            )
        })
        .map_err(|e| {
            format!("error: missing, unsafe, changed, or oversized operation file ({e:?})")
        })
}

pub fn read_run(dir: &Path, name: &str, limit: usize) -> std::result::Result<Vec<u8>, String> {
    let repo = dir
        .ancestors()
        .nth(4)
        .ok_or("error: invalid operation directory")?;
    let op = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("error: invalid operation")?;
    read_repo_file(repo, &format!("{RUN_ROOT}/{op}/{name}"), limit)
}

pub fn status(dir: &Path) -> (&'static str, Option<i64>) {
    status_with(
        dir,
        || read_run(dir, "status", STATUS_LIMIT),
        |pid| test_kill_process(pid) == Err(rustix::io::Errno::SRCH),
    )
}

pub fn status_with(
    dir: &Path,
    read_status: impl Fn() -> std::result::Result<Vec<u8>, String>,
    owner_gone: impl FnOnce(Pid) -> bool,
) -> (&'static str, Option<i64>) {
    let terminal_exit = || {
        let read = match read_status() {
            // The owner can publish the status between the secure read and
            // this presence check. Confirm the bytes before treating it as invalid.
            Err(_) if dir.join("status").symlink_metadata().is_ok() => read_status(),
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
    let pid = read_run(dir, "pid", STATUS_LIMIT)
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

pub fn age(dir: &Path) -> Option<i64> {
    age_at(dir, SystemTime::now())
}

pub fn age_at(dir: &Path, now: SystemTime) -> Option<i64> {
    [dir.join("prompt.txt"), dir.to_owned()]
        .iter()
        .find_map(|p| p.metadata().ok()?.modified().ok())
        .map(|t| match now.duration_since(t) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        })
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

fn valid_cancel_observed(dir: &Path, op: &str) -> bool {
    read_run(dir, "cancel-observed.json", STATUS_LIMIT)
        .ok()
        .and_then(|bytes| V::parse_with_depth(&bytes, 16).ok())
        .is_some_and(|v| {
            matches!(v, V::Obj(_))
                && matches!(v.get("schema"), V::Num(n) if n == "1")
                && v.get("task_id").text() == Some(op)
                && v.get("kind").text() == Some("mcp-task-cancel-observed")
        })
}

fn updated_stamp(dir: &Path, fallback: &str) -> String {
    ["status", "cancel-request.json", "run.log"]
        .iter()
        .filter_map(|name| dir.join(name).metadata().ok()?.modified().ok())
        .max()
        .map(|t| {
            DateTime::<Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
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
        if records.len() >= cap {
            break;
        }
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
        let (op_status, exit) = status(&dir);
        let cancelled = op_status == "done" && valid_cancel_observed(&dir, &name);
        let updated_at = updated_stamp(&dir, &started_at);
        let age_seconds = age_at(&dir, now);
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
            age_seconds,
            targets,
            worktree,
            title,
            caller,
            attempt_id,
        });
    }
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
    }
}
