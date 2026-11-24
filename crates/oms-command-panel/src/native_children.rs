use crate::text;
use oms_panel_core::{Store, Value, terminals};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(target_os = "linux")]
mod codex_wake;
#[cfg(target_os = "linux")]
pub(crate) use codex_wake::DayWatch as CodexDayWatch;

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(crate) struct CodexWake {
    turn: String,
    pub(crate) source: Arc<CodexDayWatch>,
}

#[cfg(target_os = "linux")]
pub(crate) struct CodexWakeRequest<'a> {
    pub(crate) previous: Option<&'a CodexWake>,
}

const TRANSCRIPT: &str = ".system_generated/logs/transcript.jsonl";
const MAX_TRANSCRIPT: usize = 4 * 1024 * 1024;
const MAX_LINE: usize = 256 * 1024;
const CODEX_CANDIDATE_LIMIT: usize = 24;
const CODEX_OLDER_PARENT_LOOKBACK_DAYS: u64 = 14;
const CODEX_OLDER_PARENT_ENTRY_BUDGET: usize = 2048;
const CODEX_READ_BUDGET: usize = 8 * 1024 * 1024;
const CODEX_RECENT_ROOT_BYTES: usize = 1024 * 1024;
const CODEX_CHAT_WINDOW: usize = 1024 * 1024;
const CODEX_USAGE_BUDGET: usize = 2 * 1024 * 1024;
const CODEX_CHAT_LIMIT: usize = 12_000;
const CODEX_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const AGY_URIS_PER_RECORD: usize = 8;
const AGY_URIS_PER_FRAME: usize = 32;
const AGY_META_LIMIT: usize = 4 * 1024;
const AGY_META_BUDGET: usize = 64 * 1024;
const AGY_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY);
pub(crate) const TASK_PREFIX: &str =
    "Task from Overall Main. Work within this task and report the result here.";

#[derive(Default)]
pub(crate) struct ObservedChildren {
    pub(crate) rows: Vec<Value>,
    pub(crate) codex: BTreeMap<String, CodexLocator>,
    #[cfg(target_os = "linux")]
    pub(crate) codex_wake: Option<CodexWake>,
}

#[derive(Clone)]
pub(crate) struct CodexLocator {
    home: PathBuf,
    root: (fs::Dev, u64),
    turn: String,
    parent: CodexAnchor,
    child: CodexAnchor,
    /// The observation clock: the chat proof re-selects children at this
    /// instant, so later interactions cannot reorder the shown rows.
    observed_ms: u64,
}

/// Identity of the bound child only: re-observing an unchanged child at a later
/// clock yields an equal locator, so an open chat survives refreshes.
impl PartialEq for CodexLocator {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            home,
            root,
            turn,
            parent,
            child,
            observed_ms: _,
        } = self;
        *home == other.home
            && *root == other.root
            && *turn == other.turn
            && *parent == other.parent
            && *child == other.child
    }
}

impl Eq for CodexLocator {}

#[derive(Clone, PartialEq, Eq)]
struct CodexAnchor {
    id: String,
    path: PathBuf,
    device: fs::Dev,
    inode: u64,
    mode: fs::RawMode,
    owner: u32,
}

#[cfg(test)]
pub(crate) fn fixture_codex_locator(tag: &str) -> CodexLocator {
    let parent = CodexAnchor {
        id: "fixture-parent".into(),
        path: "fixture-parent".into(),
        device: 0,
        inode: 0,
        mode: 0,
        owner: 0,
    };
    CodexLocator {
        home: "fixture-home".into(),
        root: (0, 0),
        turn: tag.into(),
        child: parent.clone(),
        parent,
        observed_ms: 0,
    }
}

#[cfg(test)]
thread_local!(static LEGACY_CUSTODY_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
#[cfg(test)]
thread_local!(static LEGACY_PARENT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
#[cfg(test)]
thread_local!(static LEGACY_CANDIDATE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
#[cfg(all(test, target_os = "linux"))]
thread_local!(static LEGACY_CHILD_BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
#[cfg(all(test, target_os = "linux"))]
thread_local!(static AGY_PARENT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn reset_agy_legacy_reads() {
    LEGACY_CUSTODY_CALLS.with(|n| n.set(0));
    LEGACY_PARENT_READS.with(|n| n.set(0));
    LEGACY_CANDIDATE_READS.with(|n| n.set(0));
    LEGACY_CHILD_BODY_READS.with(|n| n.set(0));
    AGY_PARENT_BYTES.with(|n| n.set(0));
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn agy_legacy_child_reads() -> usize {
    LEGACY_CHILD_BODY_READS.with(|n| n.get())
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn legacy_agy_read_index() -> (usize, usize, usize, usize) {
    (
        LEGACY_CUSTODY_CALLS.with(|n| n.get()),
        LEGACY_PARENT_READS.with(|n| n.get()),
        LEGACY_CANDIDATE_READS.with(|n| n.get()),
        LEGACY_CHILD_BODY_READS.with(|n| n.get()),
    )
}

#[cfg(test)]
fn note_legacy_custody_call(count: usize) {
    if count > 0 {
        LEGACY_CUSTODY_CALLS.with(|n| n.set(n.get() + count));
    }
}

#[cfg(test)]
fn note_legacy_parent_read(count: usize) {
    if count > 0 {
        LEGACY_PARENT_READS.with(|n| n.set(n.get() + count));
    }
}

#[cfg(test)]
fn note_legacy_candidate_read(count: usize) {
    if count > 0 {
        LEGACY_CANDIDATE_READS.with(|n| n.set(n.get() + count));
    }
}

#[cfg(not(test))]
fn note_legacy_custody_call(_count: usize) {}

#[cfg(not(test))]
fn note_legacy_parent_read(_count: usize) {}

#[cfg(not(test))]
fn note_legacy_candidate_read(_count: usize) {}

pub(crate) fn observe(
    provider: &str,
    pid: u32,
    repo: &Path,
    terminal: &str,
    agy_parent: &mut AgyParent,
    reuse_agy_roster: bool,
) -> ObservedChildren {
    let Ok(owner) = terminals::process(pid) else {
        agy_parent.clear_roster_proof();
        return ObservedChildren::default();
    };
    let observed = ObservedChildren {
        rows: match provider {
            "claude" => claude(pid, repo),
            "antigravity" => agy(
                pid,
                &owner.birth,
                repo,
                terminal,
                agy_parent,
                reuse_agy_roster,
                Instant::now(),
            ),
            _ => None,
        }
        .unwrap_or_default(),
        ..Default::default()
    };
    if terminals::process(pid).is_ok_and(|current| current == owner) {
        observed
    } else {
        agy_parent.clear_roster_proof();
        ObservedChildren::default()
    }
}

fn jsonl(store: &Store, path: &Path) -> Option<Vec<Value>> {
    Some(
        store
            .read(path, MAX_TRANSCRIPT, true)
            .ok()?
            .split(|&b| b == b'\n')
            .filter(|line| line.len() <= MAX_LINE)
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect(),
    )
}

fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn safe(s: &str, chars: usize) -> String {
    let bounded = s.chars().take(chars).collect::<String>();
    oms_panel_core::redact(bounded.as_bytes()).unwrap_or_default()
}

/// A provider-reported model exactly as published, or `None`: never trimmed, clipped or
/// redacted into a different value.
pub(crate) fn observed_model(raw: &str) -> Option<&str> {
    let placeholder = raw.starts_with('<') && raw.ends_with('>');
    let plain = !raw.is_empty()
        && raw.trim() == raw
        && raw.chars().count() <= 80
        && !raw.chars().any(char::is_control)
        && !placeholder;
    (plain && oms_panel_core::redact(raw.as_bytes()).is_ok_and(|shown| shown == raw)).then_some(raw)
}

/// The opaque row id a provider-native child is painted and bound under.
pub(crate) fn participant_id(provider: &str, id: &str) -> String {
    format!(
        "native-{provider}-{}",
        &oms_panel_core::digest(id.as_bytes())[..16]
    )
}

fn row(
    provider: &str,
    id: &str,
    title: &str,
    model: &str,
    updated: &str,
    answer: &str,
    state: &str,
) -> Value {
    json!({
        "participant": participant_id(provider, id),
        "alias": "↳",
        "role": "subagent",
        "native_child": true,
        "native_access_verification": "unverified",
        "provider": provider,
        "title": safe(title, 160),
        "model": safe(model, 80),
        "updated_at": updated,
        "created_at": updated,
        "state": state,
        "native_answer": safe(answer, 12000),
    })
}

fn claude(pid: u32, repo: &Path) -> Option<Vec<Value>> {
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    claude_in(&home, pid, repo)
}

/// Children one Claude lead pane shows: the most recently answered ones.
pub(crate) const CLAUDE_ROSTER_LIMIT: usize = 3;

/// A lead notification that independently reports this exact child finished.
pub(crate) fn claude_completion(notice: &str, agent: &str, tool: &str) -> bool {
    let head = format!(
        "<task-notification>\n<task-id>{agent}</task-id>\n<tool-use-id>{tool}</tool-use-id>\n"
    );
    let Some(rest) = notice.strip_prefix(&head) else {
        return false;
    };
    let rest = match rest.strip_prefix("<output-file>") {
        None => rest,
        Some(file) => match file.split_once("</output-file>\n") {
            Some((path, after)) if !path.contains(['<', '\n']) => after,
            _ => return false,
        },
    };
    rest.starts_with("<status>completed</status>\n")
}

/// The whole-file collector over one config root (a seam for fixtures).
pub(crate) fn claude_in(home: &Path, pid: u32, repo: &Path) -> Option<Vec<Value>> {
    let store = Store::open(home).ok()?;
    let meta = store
        .json(Path::new(&format!("sessions/{pid}.json")), 16 * 1024, true)
        .ok()??;
    let session = text(&meta, "sessionId");
    if meta["pid"].as_u64() != Some(u64::from(pid))
        || !uuid(session)
        || Path::new(text(&meta, "cwd")).canonicalize().ok().as_deref() != Some(repo)
    {
        return None;
    }
    let project_key: String = repo
        .to_str()?
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let folder = Path::new("projects").join(project_key);
    let rows = jsonl(&store, &folder.join(format!("{session}.jsonl")))?;
    if rows.is_empty() {
        return None;
    }
    let mut tools = BTreeSet::new();
    let mut children = Vec::new();
    for entry in &rows {
        if entry["sessionId"] != session {
            continue;
        }
        for block in crate::rows(&entry["message"]["content"]) {
            if block["type"] == "tool_use" && block["name"] == "Agent" {
                tools.insert(text(block, "id"));
            }
            let result = &entry["toolUseResult"];
            let id = text(result, "agentId");
            if block["type"] != "tool_result"
                || !tools.contains(text(block, "tool_use_id"))
                || !(8..=40).contains(&id.len())
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            children.push((
                id,
                result,
                text(entry, "timestamp"),
                text(block, "tool_use_id"),
            ));
        }
    }
    Some(
        children
            .into_iter()
            .rev()
            .take(CLAUDE_ROSTER_LIMIT)
            .map(|(id, result, started, tool_id)| {
                let path = folder
                    .join(session)
                    .join("subagents")
                    .join(format!("agent-{id}.jsonl"));
                let child = jsonl(&store, &path).unwrap_or_default();
                let blocks = || {
                    child
                        .iter()
                        .rev()
                        .filter(|msg| {
                            msg["type"] == "assistant"
                                && msg["agentId"] == id
                                && msg["sessionId"] == session
                        })
                        .flat_map(|msg| crate::rows(&msg["message"]["content"]).iter().rev())
                };
                let handback = blocks()
                    .find(|block| {
                        block["type"] == "tool_use" && block["name"] == "SubagentHandback"
                    })
                    .and_then(|block| block["input"]["message"].as_str())
                    .filter(|message| !message.is_empty());
                let answer = blocks()
                    .find(|block| block["type"] == "text" && !text(block, "text").is_empty())
                    .map(|block| text(block, "text"));
                let finished = rows.iter().rev().find(|msg| {
                    msg["sessionId"] == session
                        && msg["type"] == "user"
                        && msg["message"]["content"]
                            .as_str()
                            .is_some_and(|s| claude_completion(s, id, tool_id))
                });
                let delivered = finished
                    .and_then(|msg| msg["message"]["content"].as_str())
                    .and_then(|message| message.split_once("<result>"))
                    .and_then(|(_, result)| result.split_once("</result>"))
                    .map(|(result, _)| result.trim())
                    .filter(|result| !result.is_empty());
                row(
                    "claude",
                    id,
                    text(result, "description"),
                    text(result, "resolvedModel"),
                    finished
                        .map(|msg| text(msg, "timestamp"))
                        .unwrap_or(started),
                    handback.or(delivered).or(answer).unwrap_or(""),
                    if finished.is_some() {
                        "done"
                    } else {
                        "working"
                    },
                )
            })
            .collect(),
    )
}

/// The display row of a proven Claude child the whole-file collector could not
/// read (a lead transcript over its size cap). Same keys and id as that
/// collector's row; it knows no parent summary, only the spawn description.
/// `state` is only what the transcript proved: "done" or "unknown".
pub(crate) fn claude_child_row(agent: &str, title: &str, state: &str) -> Value {
    row("claude", agent, &safe(title, 80), "", "", "", state)
}

fn codex_turn(prompt: &str) -> Option<&str> {
    let assignment = prompt.strip_prefix(TASK_PREFIX)?.strip_prefix(" Turn ")?;
    let (turn, rest) = assignment.split_at_checked(20)?;
    (turn
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && rest.starts_with(". You own this provider's work."))
    .then_some(turn)
}

fn codex_current_turn(terminal: &str) -> Option<&str> {
    let prompt = terminal
        .lines()
        .rev()
        .filter_map(|line| line.trim_start().strip_prefix("› "))
        .find(|line| line.starts_with(TASK_PREFIX))?;
    codex_turn(prompt)
}

pub(crate) fn agy_current_turn(terminal: &str) -> Option<&str> {
    let prompt = terminal
        .lines()
        .rev()
        .filter_map(|line| line.trim_start().strip_prefix("> "))
        .find(|line| line.starts_with(TASK_PREFIX))?;
    codex_turn(prompt.trim_end())
}

fn codex_today() -> Option<chrono::NaiveDate> {
    Some(
        chrono::DateTime::from_timestamp(oms_panel_core::timestamp().try_into().ok()?, 0)?
            .date_naive(),
    )
}

pub(crate) fn observe_codex(
    repo: &Path,
    terminal: &str,
    home: &Path,
    root: &OwnedFd,
    stop: &AtomicBool,
    #[cfg(target_os = "linux")] wake: Option<CodexWakeRequest<'_>>,
) -> Option<ObservedChildren> {
    let turn = codex_current_turn(terminal)?;
    let mut remaining = CODEX_READ_BUDGET;
    let rollouts = codex_candidates_at(
        home.to_path_buf(),
        root.try_clone().ok()?,
        codex_today()?,
        stop,
        #[cfg(target_os = "linux")]
        wake.map(|request| codex_wake::Request {
            previous: request
                .previous
                .filter(|before| before.turn == turn)
                .map(|before| &before.source),
        }),
    )?;
    let (observed, _) = codex_rows_checked(
        &rollouts,
        repo,
        turn,
        &mut remaining,
        stop,
        codex_now_ms(),
        true,
    )?;
    #[cfg(target_os = "linux")]
    let observed = {
        let mut observed = observed;
        observed.codex_wake = rollouts.day_watch.map(|source| CodexWake {
            turn: turn.to_owned(),
            source,
        });
        observed
    };
    Some(observed)
}

pub(crate) fn same_codex_turn(before: &str, after: &str) -> bool {
    codex_current_turn(before).is_some_and(|turn| codex_current_turn(after) == Some(turn))
}

fn codex_filename_id(path: &Path) -> Option<&str> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?;
    let (stamp, id) = name.split_at_checked(name.len().checked_sub(36)?)?;
    (stamp.len() > 1 && stamp.ends_with('-') && uuid(id)).then_some(id)
}

#[derive(Clone)]
struct CodexRollout {
    path: PathBuf,
    snapshot: Stat,
}

impl CodexAnchor {
    fn new(id: &str, rollout: &CodexRollout) -> Self {
        Self {
            id: id.to_owned(),
            path: rollout.path.clone(),
            device: rollout.snapshot.st_dev,
            inode: rollout.snapshot.st_ino,
            mode: rollout.snapshot.st_mode,
            owner: rollout.snapshot.st_uid,
        }
    }
}

struct CodexRollouts {
    home: PathBuf,
    root: OwnedFd,
    today: chrono::NaiveDate,
    candidates: Vec<CodexRollout>,
    #[cfg(target_os = "linux")]
    day_watch: Option<Arc<CodexDayWatch>>,
}

fn codex_directory(root: &OwnedFd, path: &Path) -> Option<OwnedFd> {
    let mut names = path.iter();
    let first = fs::openat(root, names.next()?, CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    names
        .try_fold(first, |dir, name| {
            fs::openat(&dir, name, CODEX_DIR_FLAGS, Mode::empty())
        })
        .ok()
}

pub(crate) fn open_ambient_codex_root(home: &Path) -> Option<(PathBuf, OwnedFd)> {
    if !home.is_absolute() {
        return None;
    }
    let home = home.canonicalize().ok()?;
    let root = fs::open(&home, CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    let owner = rustix::process::geteuid().as_raw();
    (fs::fstat(&root).ok()?.st_uid == owner).then_some((home, root))
}

#[cfg(test)]
fn codex_candidates(home: &Path, today: chrono::NaiveDate) -> Option<CodexRollouts> {
    let (home, root) = open_ambient_codex_root(home)?;
    codex_candidates_at(
        home,
        root,
        today,
        &AtomicBool::new(false),
        #[cfg(target_os = "linux")]
        None,
    )
}

pub(crate) fn codex_root_identity(home: &Path, root: &OwnedFd) -> Option<(fs::Dev, u64)> {
    let opened = fs::fstat(root).ok()?;
    let named = fs::statat(fs::CWD, home, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    (opened.st_dev == named.st_dev && opened.st_ino == named.st_ino)
        .then_some((opened.st_dev, opened.st_ino))
}

fn codex_candidates_at(
    home: PathBuf,
    root: OwnedFd,
    today: chrono::NaiveDate,
    stop: &AtomicBool,
    #[cfg(target_os = "linux")] wake: Option<codex_wake::Request<'_>>,
) -> Option<CodexRollouts> {
    let owner = rustix::process::geteuid().as_raw();
    if stop.load(Ordering::Acquire) || fs::fstat(&root).ok()?.st_uid != owner {
        return None;
    }
    codex_root_identity(&home, &root)?;
    let mut days = Vec::new();
    // These directories use local dates. The adjacent UTC dates cover timezone offsets.
    for date in [today.pred_opt(), Some(today), today.succ_opt()]
        .into_iter()
        .flatten()
    {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let path = Path::new("sessions").join(date.format("%Y/%m/%d").to_string());
        if let Some(dir) = codex_directory(&root, &path) {
            days.push((path, dir));
        }
    }
    // Arm on the same directory descriptors before enumeration. A later creation
    // can be missed by the scan only if its events are already eligible to wake.
    #[cfg(target_os = "linux")]
    let day_watch = wake.and_then(|request| CodexDayWatch::prepare(&home, &root, &days, request));
    let mut candidates = Vec::new();
    for (path, dir) in days {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let Ok(entries) = Dir::read_from(&dir) else {
            continue;
        };
        for entry in entries.take(2048).flatten() {
            if stop.load(Ordering::Acquire) {
                return None;
            }
            let name = entry.file_name();
            let Ok(filename) = name.to_str() else {
                continue;
            };
            if codex_filename_id(Path::new(filename)).is_none() {
                continue;
            }
            if let Ok(stat) = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
                && FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_uid == owner
                && stat.st_nlink == 1
                && stat.st_size >= 0
            {
                candidates.push(CodexRollout {
                    path: path.join(filename),
                    snapshot: stat,
                });
            }
        }
    }
    candidates.sort_unstable_by(|a, b| {
        (b.snapshot.st_mtime, b.snapshot.st_mtime_nsec, &b.path).cmp(&(
            a.snapshot.st_mtime,
            a.snapshot.st_mtime_nsec,
            &a.path,
        ))
    });
    candidates.truncate(CODEX_CANDIDATE_LIMIT);
    Some(CodexRollouts {
        home,
        root,
        today,
        candidates,
        #[cfg(target_os = "linux")]
        day_watch,
    })
}

fn codex_same_identity(expected: &Stat, actual: &Stat) -> bool {
    expected.st_dev == actual.st_dev
        && expected.st_ino == actual.st_ino
        && expected.st_mode == actual.st_mode
        && expected.st_uid == actual.st_uid
        && expected.st_nlink == actual.st_nlink
}

fn codex_same_version(expected: &Stat, actual: &Stat) -> bool {
    expected.st_size == actual.st_size
        && expected.st_mtime == actual.st_mtime
        && expected.st_mtime_nsec == actual.st_mtime_nsec
        && expected.st_ctime == actual.st_ctime
        && expected.st_ctime_nsec == actual.st_ctime_nsec
}

impl CodexRollouts {
    fn open(&self, rollout: &CodexRollout) -> Option<(File, Stat)> {
        let parent = codex_directory(&self.root, rollout.path.parent()?)?;
        let name = rollout.path.file_name()?;
        let named = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        if !codex_same_identity(&rollout.snapshot, &named) {
            return None;
        }
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let fd = fs::openat(&parent, name, flags, Mode::empty()).ok()?;
        let sampled = fs::fstat(&fd).ok()?;
        codex_same_identity(&rollout.snapshot, &sampled).then(|| (File::from(fd), sampled))
    }

    fn status(&self, rollout: &CodexRollout, file: &File) -> Option<Stat> {
        let parent = codex_directory(&self.root, rollout.path.parent()?)?;
        let named = fs::statat(
            &parent,
            rollout.path.file_name()?,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .ok()?;
        let opened = fs::fstat(file).ok()?;
        (codex_same_identity(&rollout.snapshot, &named)
            && codex_same_identity(&rollout.snapshot, &opened))
        .then_some(opened)
    }

    fn confirm_snapshot(
        &self,
        rollout: &CodexRollout,
        file: &mut File,
        sampled: &Stat,
        ranges: &[(u64, &[u8])],
        remaining: &mut usize,
    ) -> Option<bool> {
        let Some(before) = self.status(rollout, file) else {
            return Some(false);
        };
        if codex_same_version(sampled, &before) {
            return Some(true);
        }
        if before.st_size <= sampled.st_size {
            return Some(false);
        }
        // Growth alone cannot prove that bytes in the sampled range stayed intact.
        for &(offset, original) in ranges {
            let Ok(rechecked) = codex_read_range(file, offset, original.len(), remaining)? else {
                return Some(false);
            };
            if rechecked != original {
                return Some(false);
            }
        }
        Some(
            self.status(rollout, file)
                .is_some_and(|after| codex_same_version(&before, &after)),
        )
    }

    fn root_unchanged(&self) -> Option<(fs::Dev, u64)> {
        codex_root_identity(&self.home, &self.root)
    }
}

fn codex_read_header(file: &mut File, size: u64, remaining: &mut usize) -> Option<Vec<u8>> {
    let bound = size.min((MAX_LINE + 1) as u64) as usize;
    let cap = bound.min(*remaining);
    let mut bytes = Vec::new();
    let mut reader = BufReader::new(file.take(cap as u64));
    let read = reader.read_until(b'\n', &mut bytes);
    // Take sees all descriptor reads, including buffered read-ahead.
    *remaining -= cap - reader.into_inner().limit() as usize;
    let length = read.ok()?;
    (bytes.ends_with(b"\n") || length == bound).then_some(bytes)
}

// None means the shared budget cannot cover this exact read.
fn codex_read_range(
    file: &mut File,
    offset: u64,
    len: usize,
    remaining: &mut usize,
) -> Option<std::io::Result<Vec<u8>>> {
    if len > *remaining {
        return None;
    }
    Some(file.seek(SeekFrom::Start(offset)).and_then(|_| {
        let mut bytes = vec![0; len];
        let mut reader = file.take(len as u64);
        let read = reader.read_exact(&mut bytes);
        *remaining -= len - reader.limit() as usize;
        read.map(|()| bytes)
    }))
}

fn codex_header(bytes: &[u8], id: &str, repo: &Path) -> Option<Value> {
    oms_panel_core::decode(bytes.strip_suffix(b"\n")?, MAX_LINE)
        .ok()
        .filter(|header| {
            let meta = &header["payload"];
            header["type"] == "session_meta" && meta["id"] == id && codex_repo(meta, repo)
        })
}

fn codex_repo(value: &Value, repo: &Path) -> bool {
    let cwd = Path::new(text(value, "cwd"));
    cwd.is_absolute() && cwd.canonicalize().ok().as_deref() == Some(repo)
}

fn codex_spawn(header: &Value) -> &Value {
    &header["payload"]["source"]["subagent"]["thread_spawn"]
}

fn codex_child_title(spawn: &Value) -> &str {
    let nickname = text(spawn, "agent_nickname").trim();
    if !nickname.is_empty()
        && nickname.len() <= 160
        && crate::render::clean(nickname, 160) == nickname
    {
        nickname
    } else {
        "Native subagent"
    }
}

fn codex_user_message(event: &Value, session: &str) -> bool {
    let payload = &event["payload"];
    event["type"] == "event_msg"
        && payload["type"] == "item_completed"
        && payload["thread_id"] == session
        && payload["item"]["type"] == "UserMessage"
}

fn codex_assignment(event: &Value, session: &str, turn: &str) -> bool {
    codex_user_message(event, session)
        && crate::rows(&event["payload"]["item"]["content"])
            .iter()
            .any(|block| block["type"] == "text" && codex_turn(text(block, "text")) == Some(turn))
}

fn codex_complete_records(body: &[u8], recent_only: bool) -> impl Iterator<Item = &[u8]> {
    let start = if recent_only {
        body.iter()
            .position(|&byte| byte == b'\n')
            .map_or(body.len(), |newline| newline + 1)
    } else {
        0
    };
    body[start..]
        .split_inclusive(|&byte| byte == b'\n')
        .filter_map(|line| line.strip_suffix(b"\n"))
}

fn codex_has_assignment(body: &[u8], recent_only: bool, id: &str, turn: &str) -> bool {
    codex_complete_records(body, recent_only)
        .filter(|line| {
            line.len() <= MAX_LINE
                && std::str::from_utf8(line).is_ok_and(|line| line.contains(turn))
        })
        .filter_map(|line| oms_panel_core::decode(line, MAX_LINE).ok())
        .any(|event| codex_assignment(&event, id, turn))
}

const CODEX_SEEN_WINDOW_MS: u64 = 120_000;
const CODEX_SEEN_SKEW_MS: u64 = 5_000;
/// 2020-01-01T00:00:00Z: older stamps are not a Codex clock.
const CODEX_SEEN_FLOOR_MS: u64 = 1_577_836_800_000;

pub(crate) fn codex_recent_age(at: u64, observed: u64) -> Option<u64> {
    let age = observed.saturating_sub(at);
    (at <= observed.saturating_add(CODEX_SEEN_SKEW_MS) && age <= CODEX_SEEN_WINDOW_MS)
        .then_some(age)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CodexChildRecency {
    Created(u64),
    Interacted(u64),
}

#[derive(Default)]
struct CodexTurnActivity {
    completed_at: Option<u64>,
    interactions: BTreeMap<String, u64>,
}

/// Latest valid `interacted` completion (epoch ms) per child thread, from the
/// already sampled parent bytes: only records after the current-turn
/// UserMessage that share its parent `thread_id` and `turn_id` count. That
/// UserMessage must occur exactly once with a usable id; a repeat (even with
/// the same id) leaves no signal at all. Its typed completion clock, when
/// present, is the lower bound for showing newly created children until a
/// later typed user message from this parent supersedes it.
fn codex_turn_activity(
    body: &[u8],
    recent_only: bool,
    parent: &str,
    turn: &str,
    now_ms: u64,
) -> CodexTurnActivity {
    let mut turn_id: Option<String> = None;
    let mut activity = CodexTurnActivity::default();
    let records = codex_complete_records(body, recent_only)
        .filter(|line| line.len() <= MAX_LINE)
        .filter_map(|line| std::str::from_utf8(line).ok());
    for line in records {
        let activity_record = line.contains(turn) || line.contains("SubAgentActivity");
        let maybe_later_input = activity.completed_at.is_some()
            && (line.contains("UserMessage") || line.contains(r"\u"));
        if !activity_record && !maybe_later_input {
            continue;
        }
        let Ok(event) = oms_panel_core::decode(line.as_bytes(), MAX_LINE) else {
            continue;
        };
        let payload = &event["payload"];
        if activity_record && codex_assignment(&event, parent, turn) {
            match payload["turn_id"].as_str() {
                Some(id) if turn_id.is_none() && codex_rollout_identifier(id) => {
                    turn_id = Some(id.to_owned());
                    activity.completed_at = payload["completed_at_ms"].as_u64().filter(|&at| {
                        at >= CODEX_SEEN_FLOOR_MS && at <= now_ms.saturating_add(CODEX_SEEN_SKEW_MS)
                    });
                }
                _ => return CodexTurnActivity::default(),
            }
            continue;
        }
        if activity.completed_at.is_some()
            && codex_user_message(&event, parent)
            && payload["turn_id"]
                .as_str()
                .is_some_and(codex_rollout_identifier)
        {
            activity.completed_at = None;
        }
        if !activity_record {
            continue;
        }
        let item = &payload["item"];
        let (Some(turn_id), Some(child)) = (turn_id.as_deref(), item["agent_thread_id"].as_str())
        else {
            continue;
        };
        if event["type"] != "event_msg"
            || payload["type"] != "item_completed"
            || payload["thread_id"] != parent
            || payload["turn_id"] != turn_id
            || item["type"] != "SubAgentActivity"
            || item["kind"] != "interacted"
            || !uuid(child)
        {
            continue;
        }
        let (Some(started), Some(completed)) = (
            payload["started_at_ms"].as_u64(),
            payload["completed_at_ms"].as_u64(),
        ) else {
            continue;
        };
        if started >= CODEX_SEEN_FLOOR_MS
            && started <= completed
            && completed <= now_ms.saturating_add(CODEX_SEEN_SKEW_MS)
        {
            let latest = activity.interactions.entry(child.to_owned()).or_insert(0);
            *latest = (*latest).max(completed);
        }
    }
    activity
}

struct CodexChildHeader<'a> {
    id: &'a str,
    rollout: &'a CodexRollout,
    parent: String,
    started: String,
    created_at: Option<u64>,
    title: String,
}

struct CodexParentSample {
    rollout: CodexRollout,
    file: File,
    sampled: Stat,
    header: Vec<u8>,
    offset: u64,
    body: Vec<u8>,
}

fn codex_parent_source(meta: &Value) -> bool {
    matches!(meta["source"].as_str(), Some("vscode" | "cli")) && meta["thread_source"] == "user"
}

fn codex_uuidv7_date(id: &str) -> Option<chrono::NaiveDate> {
    let bytes = id.as_bytes();
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(i, &b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        || bytes[14] != b'7'
        || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
    {
        return None;
    }
    let hi = u64::from_str_radix(&id[0..8], 16).ok()?;
    let lo = u64::from_str_radix(&id[9..13], 16).ok()?;
    let ms = i64::try_from((hi << 16) | lo).ok()?;
    Some(chrono::DateTime::from_timestamp_millis(ms)?.date_naive())
}

fn codex_older_parent_candidates(
    rollouts: &CodexRollouts,
    needed: &BTreeSet<&str>,
    stop: &AtomicBool,
) -> Option<Vec<CodexRollout>> {
    if stop.load(Ordering::Acquire) {
        return None;
    }
    if needed.is_empty() {
        return Some(Vec::new());
    }
    let mut window = Vec::with_capacity(CODEX_OLDER_PARENT_LOOKBACK_DAYS as usize);
    let mut cursor = rollouts.today.pred_opt().and_then(|day| day.pred_opt());
    for _ in 0..CODEX_OLDER_PARENT_LOOKBACK_DAYS {
        let Some(date) = cursor else {
            break;
        };
        window.push(date);
        cursor = date.pred_opt();
    }
    let (Some(&newest), Some(&oldest)) = (window.first(), window.last()) else {
        return Some(Vec::new());
    };
    let mut hinted_dates: BTreeMap<chrono::NaiveDate, BTreeSet<&str>> = BTreeMap::new();
    let mut unhinted: BTreeSet<&str> = BTreeSet::new();
    for &id in needed {
        if let Some(hint_day) = codex_uuidv7_date(id) {
            for date in [hint_day.pred_opt(), Some(hint_day), hint_day.succ_opt()]
                .into_iter()
                .flatten()
                .filter(|date| (oldest..=newest).contains(date))
            {
                hinted_dates.entry(date).or_default().insert(id);
            }
        } else {
            unhinted.insert(id);
        }
    }
    if hinted_dates.is_empty() && unhinted.is_empty() {
        return Some(Vec::new());
    }
    let owner = rustix::process::geteuid().as_raw();
    if fs::fstat(&rollouts.root).ok()?.st_uid != owner {
        return None;
    }
    rollouts.root_unchanged()?;
    let mut found = BTreeMap::new();
    let mut entries_remaining = CODEX_OLDER_PARENT_ENTRY_BUDGET;
    let mut truncated = false;
    for date in window {
        if unhinted.is_empty() && !hinted_dates.contains_key(&date) {
            continue;
        }
        if entries_remaining == 0 {
            truncated = true;
            break;
        }
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let path = Path::new("sessions").join(date.format("%Y/%m/%d").to_string());
        let Some(dir) = codex_directory(&rollouts.root, &path) else {
            continue;
        };
        let Ok(entries) = Dir::read_from(&dir) else {
            continue;
        };
        for entry in entries {
            if entries_remaining == 0 {
                truncated = true;
                break;
            }
            entries_remaining -= 1;
            if stop.load(Ordering::Acquire) {
                return None;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name();
            let Ok(filename) = name.to_str() else {
                continue;
            };
            let Some(id) = codex_filename_id(Path::new(filename)) else {
                continue;
            };
            if !needed.contains(id) {
                continue;
            }
            if let Ok(stat) = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
                && FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_uid == owner
                && stat.st_nlink == 1
                && stat.st_size >= 0
            {
                let rollout = CodexRollout {
                    path: path.join(filename),
                    snapshot: stat,
                };
                if found.insert(id.to_owned(), rollout).is_some() {
                    return None;
                }
            }
        }
        if truncated {
            break;
        }
    }
    if truncated && !found.is_empty() {
        return None;
    }
    Some(found.into_values().collect())
}

#[allow(clippy::too_many_arguments)]
fn codex_verify_parent(
    rollouts: &CodexRollouts,
    rollout: &CodexRollout,
    mut file: File,
    sampled: Stat,
    size: u64,
    header_bytes: Vec<u8>,
    id: &str,
    turn: &str,
    remaining: &mut usize,
    stop: &AtomicBool,
) -> Option<Option<CodexParentSample>> {
    // Every eligible root's scan must fit the shared budget to prove uniqueness.
    let recent_only = size > MAX_TRANSCRIPT as u64;
    let offset = if recent_only {
        // Probe the byte before the suffix for an LF at the boundary.
        size - CODEX_RECENT_ROOT_BYTES as u64 - 1
    } else {
        header_bytes.len() as u64
    };
    let len = usize::try_from(size.checked_sub(offset)?).ok()?;
    if stop.load(Ordering::Acquire) {
        return None;
    }
    let body = codex_read_range(&mut file, offset, len, remaining)?.ok()?;
    let matched = codex_has_assignment(&body, recent_only, id, turn);
    if !rollouts.confirm_snapshot(
        rollout,
        &mut file,
        &sampled,
        &[(0, &header_bytes), (offset, &body)],
        remaining,
    )? {
        return None;
    }
    Some(matched.then(|| CodexParentSample {
        rollout: rollout.clone(),
        file,
        sampled,
        header: header_bytes,
        offset,
        body,
    }))
}

#[cfg(test)]
fn codex_rows(
    rollouts: &CodexRollouts,
    repo: &Path,
    turn: &str,
    remaining: &mut usize,
) -> Option<(ObservedChildren, CodexParentSample)> {
    codex_rows_at(rollouts, repo, turn, remaining, codex_now_ms())
}

#[cfg(test)]
fn codex_rows_at(
    rollouts: &CodexRollouts,
    repo: &Path,
    turn: &str,
    remaining: &mut usize,
    now_ms: u64,
) -> Option<(ObservedChildren, CodexParentSample)> {
    codex_rows_checked(
        rollouts,
        repo,
        turn,
        remaining,
        &AtomicBool::new(false),
        now_ms,
        true,
    )
}

fn codex_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn codex_rows_checked(
    rollouts: &CodexRollouts,
    repo: &Path,
    turn: &str,
    remaining: &mut usize,
    stop: &AtomicBool,
    now_ms: u64,
    project_usage: bool,
) -> Option<(ObservedChildren, CodexParentSample)> {
    let mut parent: Option<(String, CodexParentSample)> = None;
    let mut children = Vec::new();
    let mut recent_ids = BTreeSet::new();
    for rollout in &rollouts.candidates {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let Some(id) = codex_filename_id(&rollout.path) else {
            continue;
        };
        recent_ids.insert(id);
        // An unreadable selected header could belong to another eligible root.
        let (mut file, sampled) = rollouts.open(rollout)?;
        let size = u64::try_from(sampled.st_size).ok()?;
        // A fork can repeat an inherited session header after its own first line.
        let header_bytes = codex_read_header(&mut file, size, remaining)?;
        #[cfg(target_os = "linux")]
        if let Some(source) = &rollouts.day_watch {
            source.record_header(
                &rollout.path,
                !header_bytes.ends_with(b"\n") && size < MAX_LINE as u64,
            );
        }
        let Some(header) = codex_header(&header_bytes, id, repo) else {
            rollouts
                .confirm_snapshot(
                    rollout,
                    &mut file,
                    &sampled,
                    &[(0, &header_bytes)],
                    remaining,
                )?
                .then_some(())?;
            continue;
        };
        let meta = &header["payload"];
        let spawn = codex_spawn(&header);
        let spawned_by = text(spawn, "parent_thread_id");
        if uuid(spawned_by) && spawned_by != id {
            if !rollouts.confirm_snapshot(
                rollout,
                &mut file,
                &sampled,
                &[(0, &header_bytes)],
                remaining,
            )? {
                continue;
            }
            let started = chrono::DateTime::parse_from_rfc3339(text(&header, "timestamp"))
                .map(|stamp| stamp.to_rfc3339())
                .unwrap_or_default();
            // The envelope clock records materialization. SessionMeta's own
            // clock was captured when the provider created this rollout.
            let created_at = chrono::DateTime::parse_from_rfc3339(text(meta, "timestamp"))
                .ok()
                .and_then(|stamp| u64::try_from(stamp.timestamp_millis()).ok());
            children.push(CodexChildHeader {
                id,
                rollout,
                parent: spawned_by.to_owned(),
                started,
                created_at,
                title: codex_child_title(spawn).to_owned(),
            });
        } else if codex_parent_source(meta) {
            if let Some(sample) = codex_verify_parent(
                rollouts,
                rollout,
                file,
                sampled,
                size,
                header_bytes,
                id,
                turn,
                remaining,
                stop,
            )? && parent.replace((id.to_owned(), sample)).is_some()
            {
                return None;
            }
        } else {
            rollouts
                .confirm_snapshot(
                    rollout,
                    &mut file,
                    &sampled,
                    &[(0, &header_bytes)],
                    remaining,
                )?
                .then_some(())?;
        }
    }
    let missing_parents: BTreeSet<&str> = children
        .iter()
        .map(|child| child.parent.as_str())
        .filter(|&by| !recent_ids.contains(by))
        .collect();
    for rollout in codex_older_parent_candidates(rollouts, &missing_parents, stop)? {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let Some(id) = codex_filename_id(&rollout.path) else {
            continue;
        };
        let (mut file, sampled) = rollouts.open(&rollout)?;
        let size = u64::try_from(sampled.st_size).ok()?;
        let header_bytes = codex_read_header(&mut file, size, remaining)?;
        let Some(header) = codex_header(&header_bytes, id, repo) else {
            rollouts
                .confirm_snapshot(
                    &rollout,
                    &mut file,
                    &sampled,
                    &[(0, &header_bytes)],
                    remaining,
                )?
                .then_some(())?;
            continue;
        };
        if codex_parent_source(&header["payload"]) {
            if let Some(sample) = codex_verify_parent(
                rollouts,
                &rollout,
                file,
                sampled,
                size,
                header_bytes,
                id,
                turn,
                remaining,
                stop,
            )? && parent.replace((id.to_owned(), sample)).is_some()
            {
                return None;
            }
        } else {
            rollouts
                .confirm_snapshot(
                    &rollout,
                    &mut file,
                    &sampled,
                    &[(0, &header_bytes)],
                    remaining,
                )?
                .then_some(())?;
        }
    }
    let root = rollouts.root_unchanged()?;
    let (parent_id_owned, mut parent_sample) = parent?;
    let parent_id = parent_id_owned.as_str();
    let parent = CodexAnchor::new(parent_id, &parent_sample.rollout);
    let mut counts = BTreeMap::new();
    for child in &children {
        *counts.entry(child.id).or_insert(0) += 1;
    }
    let recent_only = u64::try_from(parent_sample.sampled.st_size).ok()? > MAX_TRANSCRIPT as u64;
    let activity = codex_turn_activity(&parent_sample.body, recent_only, parent_id, turn, now_ms);
    let recent_interaction = |child: &CodexChildHeader<'_>| {
        activity
            .interactions
            .get(child.id)
            .copied()
            .filter(|&completed| {
                counts[child.id] == 1 && codex_recent_age(completed, now_ms).is_some()
            })
    };
    let recent_creation = |child: &CodexChildHeader<'_>| {
        let created = child.created_at?;
        let completed = activity.completed_at?;
        (counts[child.id] == 1
            && completed <= created
            && codex_recent_age(created, now_ms).is_some())
        .then_some(created)
    };
    let mut seen = BTreeSet::new();
    let children: Vec<_> = children
        .into_iter()
        .filter(|child| child.parent == parent_id && seen.insert(child.id))
        .collect();
    // Interaction outranks creation. Both use newest first, then header order.
    // Chat re-resolution uses this same selection at its saved observation clock.
    let mut ranked: Vec<_> = children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            let recency = recent_interaction(child)
                .map(CodexChildRecency::Interacted)
                .or_else(|| recent_creation(child).map(CodexChildRecency::Created))?;
            Some((index, recency))
        })
        .collect();
    ranked.sort_by_key(|&(_, recency)| std::cmp::Reverse(recency));
    let mut picked: Vec<_> = ranked.into_iter().take(3).map(|(index, _)| index).collect();
    let history: Vec<_> = (0..children.len())
        .filter(|index| !picked.contains(index))
        .collect();
    picked.extend(history);
    picked.truncate(3);
    let mut seen_participants = BTreeSet::new();
    let mut observed = ObservedChildren::default();
    // Reserve exact sampled ranges for a possible parent append proof.
    let proof_reserve = parent_sample
        .header
        .len()
        .checked_add(parent_sample.body.len())?;
    let mut usage_budget = remaining
        .saturating_sub(proof_reserve)
        .min(CODEX_USAGE_BUDGET);
    let usage_initial = usage_budget;
    for child in picked.into_iter().map(|index| &children[index]) {
        let id = child.id;
        let mut child_row = row("codex", id, &child.title, "", &child.started, "", "unknown");
        if !child.started.is_empty() {
            child_row["started_at"] = json!(child.started);
        }
        if project_usage
            && counts[id] == 1
            && let Some(usage) = codex_child_usage(
                rollouts,
                child.rollout,
                repo,
                id,
                parent_id,
                &parent_sample.header,
                &mut usage_budget,
            )
        {
            child_row["native_response_usage"] = usage.0;
            if let Some(model) = usage.1.as_deref() {
                child_row["observed_model"] = json!(model);
            }
        }
        let participant = text(&child_row, "participant").to_owned();
        let participant = participant.as_str();
        if !seen_participants.insert(participant.to_owned()) {
            observed.codex.remove(participant);
            child_row
                .as_object_mut()
                .unwrap()
                .remove("native_response_usage");
            child_row.as_object_mut().unwrap().remove("observed_model");
            for earlier in &mut observed.rows {
                if text(earlier, "participant") == participant
                    && let Some(fields) = earlier.as_object_mut()
                {
                    fields.remove("native_response_usage");
                    fields.remove("observed_model");
                    fields.remove("seen_at");
                    fields.remove("seen_observed_ms");
                    fields.remove("codex_recent_creation");
                }
            }
        } else if counts[id] == 1 {
            // "Seen" is a recency signal only; `state` stays "unknown".
            if let Some(completed) = recent_interaction(child) {
                child_row["seen_at"] = json!(completed);
                child_row["seen_observed_ms"] = json!(now_ms);
            }
            if let Some(created) = recent_creation(child) {
                child_row["codex_recent_creation"] =
                    json!({"at_ms": created, "observed_ms": now_ms});
            }
            observed.codex.insert(
                participant.to_owned(),
                CodexLocator {
                    home: rollouts.home.clone(),
                    root,
                    turn: turn.to_owned(),
                    parent: parent.clone(),
                    child: CodexAnchor::new(id, child.rollout),
                    observed_ms: now_ms,
                },
            );
        }
        observed.rows.push(child_row);
    }
    *remaining -= usage_initial - usage_budget;
    let has_usage = observed
        .rows
        .iter()
        .any(|row| row.get("native_response_usage").is_some());
    if rollouts.root_unchanged()? != root {
        return None;
    }
    let mut parent_now = rollouts.status(&parent_sample.rollout, &parent_sample.file)?;
    if has_usage {
        let confirmed = rollouts
            .confirm_snapshot(
                &parent_sample.rollout,
                &mut parent_sample.file,
                &parent_sample.sampled,
                &[
                    (0, &parent_sample.header),
                    (parent_sample.offset, &parent_sample.body),
                ],
                remaining,
            )
            .unwrap_or(false);
        if rollouts.root_unchanged()? != root {
            return None;
        }
        parent_now = rollouts.status(&parent_sample.rollout, &parent_sample.file)?;
        if !confirmed {
            for row in &mut observed.rows {
                let fields = row.as_object_mut().unwrap();
                fields.remove("native_response_usage");
                fields.remove("observed_model");
            }
        }
    }
    // An append can add another matching Turn record outside the sampled range.
    // Keep the ordinary row and renew creation evidence on the next observation.
    if !codex_same_version(&parent_sample.sampled, &parent_now) {
        for row in &mut observed.rows {
            row.as_object_mut().unwrap().remove("codex_recent_creation");
        }
    }
    (!stop.load(Ordering::Acquire)).then_some((observed, parent_sample))
}

#[cfg(test)]
thread_local!(static CODEX_TEST_APPEND_PARENT: std::cell::RefCell<Option<(PathBuf, bool)>> = const { std::cell::RefCell::new(None) });

fn codex_child_usage(
    rollouts: &CodexRollouts,
    rollout: &CodexRollout,
    repo: &Path,
    id: &str,
    parent: &str,
    parent_header: &[u8],
    remaining: &mut usize,
) -> Option<(Value, Option<String>)> {
    #[cfg(test)]
    CODEX_TEST_APPEND_PARENT.with(|path| {
        if let Some((path, alter)) = path.borrow_mut().take() {
            use std::io::Write;
            if alter {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.write_all(b" ").unwrap();
            }
            writeln!(
                std::fs::OpenOptions::new().append(true).open(path).unwrap(),
                "{{}} "
            )
            .unwrap();
        }
    });
    let (mut file, sampled) = rollouts.open(rollout)?;
    let size = u64::try_from(sampled.st_size).ok()?;
    let header_bytes = codex_read_header(&mut file, size, remaining)?;
    let header = codex_header(&header_bytes, id, repo)?;
    if codex_spawn(&header)["parent_thread_id"] != parent {
        return None;
    }
    let session = text(&header["payload"], "session_id");
    if !uuid(session)
        || !oms_panel_core::decode(parent_header, MAX_LINE)
            .is_ok_and(|meta| meta["payload"]["session_id"] == session)
    {
        return None;
    }
    let header_end = header_bytes.len() as u64;
    let recent_only = size.checked_sub(header_end)? > CODEX_CHAT_WINDOW as u64;
    let offset = if recent_only {
        size - CODEX_CHAT_WINDOW as u64 - 1
    } else {
        header_end
    };
    let len = usize::try_from(size.checked_sub(offset)?).ok()?;
    let body = codex_read_range(&mut file, offset, len, remaining)?.ok()?;
    let (_, usage) = codex_history_parts(&body, recent_only, repo, id, Some(session), false);
    if !rollouts.confirm_snapshot(
        rollout,
        &mut file,
        &sampled,
        &[(0, &header_bytes), (offset, &body)],
        remaining,
    )? {
        return None;
    }
    let observation = usage?;
    let counters = |v: &Value| {
        let u = CodexResponseUsage::from_counters(v)?;
        Some(
            json!({"input_tokens":u.input, "output_tokens":u.output, "cached_input_tokens":u.cache_read, "cache_write_input_tokens":u.cache_write, "reasoning_output_tokens":u.reasoning, "total_tokens":u.total}),
        )
    };
    let usage = json!({"response": counters(&observation.payload["usage"])?,
        "turn": counters(&observation.payload["turn_token_usage"])?,
        "thread": counters(&observation.payload["thread_token_usage"])?});
    Some((usage, observation.model))
}

pub(crate) fn codex_chat_turn_matches(terminal: &str, expected: &CodexLocator) -> bool {
    codex_current_turn(terminal) == Some(expected.turn.as_str())
}

pub(crate) fn codex_chat(
    repo: &Path,
    terminal: &str,
    expected: &CodexLocator,
    home: &Path,
    root: &OwnedFd,
    stop: &AtomicBool,
) -> Option<String> {
    if stop.load(Ordering::Acquire)
        || !codex_chat_turn_matches(terminal, expected)
        || home != expected.home
        || codex_root_identity(home, root)? != expected.root
    {
        return None;
    }
    let rollouts = codex_candidates_at(
        home.to_path_buf(),
        root.try_clone().ok()?,
        codex_today()?,
        stop,
        #[cfg(target_os = "linux")]
        None,
    )?;
    codex_chat_checked(&rollouts, repo, &expected.turn, expected, stop)
}

#[cfg(test)]
fn codex_chat_from(
    rollouts: &CodexRollouts,
    repo: &Path,
    turn: &str,
    expected: &CodexLocator,
) -> Option<String> {
    codex_chat_checked(rollouts, repo, turn, expected, &AtomicBool::new(false))
}

fn codex_chat_checked(
    rollouts: &CodexRollouts,
    repo: &Path,
    turn: &str,
    expected: &CodexLocator,
    stop: &AtomicBool,
) -> Option<String> {
    if stop.load(Ordering::Acquire)
        || turn != expected.turn
        || rollouts.home != expected.home
        || rollouts.root_unchanged()? != expected.root
    {
        return None;
    }
    let mut remaining = CODEX_READ_BUDGET;
    let (current, mut parent) = codex_rows_checked(
        rollouts,
        repo,
        turn,
        &mut remaining,
        stop,
        expected.observed_ms,
        false,
    )?;
    if stop.load(Ordering::Acquire) || !current.codex.values().any(|locator| locator == expected) {
        return None;
    }
    let child = rollouts
        .candidates
        .iter()
        .find(|candidate| candidate.path == expected.child.path)?;
    let (mut file, sampled) = rollouts.open(child)?;
    let size = u64::try_from(sampled.st_size).ok()?;
    let header_bytes = codex_read_header(&mut file, size, &mut remaining)?;
    let header = codex_header(&header_bytes, &expected.child.id, repo)?;
    if codex_spawn(&header)["parent_thread_id"] != expected.parent.id {
        return None;
    }
    let header_end = header_bytes.len() as u64;
    let recent_only = size.checked_sub(header_end)? > CODEX_CHAT_WINDOW as u64;
    let offset = if recent_only {
        size - CODEX_CHAT_WINDOW as u64 - 1
    } else {
        header_end
    };
    let len = usize::try_from(size.checked_sub(offset)?).ok()?;
    if stop.load(Ordering::Acquire) {
        return None;
    }
    let body = codex_read_range(&mut file, offset, len, &mut remaining)?.ok()?;
    let session = text(&header["payload"], "session_id");
    let usage_session = (uuid(session)
        && oms_panel_core::decode(&parent.header, MAX_LINE)
            .is_ok_and(|meta| meta["payload"]["session_id"] == session))
    .then_some(session);
    let history = codex_history(&body, recent_only, repo, &expected.child.id, usage_session)?;
    rollouts
        .confirm_snapshot(
            child,
            &mut file,
            &sampled,
            &[(0, &header_bytes), (offset, &body)],
            &mut remaining,
        )?
        .then_some(())?;
    (rollouts.confirm_snapshot(
        &parent.rollout,
        &mut parent.file,
        &parent.sampled,
        &[(0, &parent.header), (parent.offset, &parent.body)],
        &mut remaining,
    )? && rollouts.root_unchanged()? == expected.root
        && !stop.load(Ordering::Acquire))
    .then_some(history)
}

fn codex_rollout_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

struct CodexResponseUsage {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    total: i64,
}

impl CodexResponseUsage {
    fn parse(payload: &Value) -> Option<Self> {
        if !codex_rollout_identifier(text(payload, "turn_id"))
            || !codex_rollout_identifier(text(payload, "root_turn_id"))
        {
            return None;
        }
        Self::from_counters(&payload["turn_token_usage"])?;
        Self::from_counters(&payload["thread_token_usage"])?;
        Self::from_counters(&payload["usage"])
    }

    fn from_counters(usage: &Value) -> Option<Self> {
        let count = |name| usage[name].as_i64().filter(|count| *count >= 0);
        Some(Self {
            input: count("input_tokens")?,
            output: count("output_tokens")?,
            cache_read: count("cached_input_tokens")?,
            cache_write: count("cache_write_input_tokens")?,
            reasoning: count("reasoning_output_tokens")?,
            total: count("total_tokens")?,
        })
    }

    fn line(&self) -> String {
        // Codex maps omitted input and output token detail to zeros.
        let detail = |count: i64| {
            if count == 0 {
                "?".to_owned()
            } else {
                count.to_string()
            }
        };
        format!(
            "Codex recorded response usage · in {} · out {} · cache {}/{} · reasoning {} · total {}",
            self.input,
            self.output,
            detail(self.cache_read),
            detail(self.cache_write),
            detail(self.reasoning),
            self.total
        )
    }
}

struct CodexUsageObservation {
    payload: Value,
    usage: CodexResponseUsage,
    model: Option<String>,
}

fn codex_history(
    body: &[u8],
    recent_only: bool,
    repo: &Path,
    child: &str,
    usage_session: Option<&str>,
) -> Option<String> {
    let (history, observation) =
        codex_history_parts(body, recent_only, repo, child, usage_session, true);
    let history = chat_tail(&history)?;
    let Some(observation) = observation else {
        return Some(history);
    };
    let line = observation.usage.line();
    let start = history
        .ceil_char_boundary((history.len() + 2 + line.len()).saturating_sub(CODEX_CHAT_LIMIT));
    Some(format!("{}\n\n{line}", history[start..].trim_start()))
}

fn codex_history_parts(
    body: &[u8],
    recent_only: bool,
    repo: &Path,
    child: &str,
    mut usage_session: Option<&str>,
    collect_chat: bool,
) -> (String, Option<CodexUsageObservation>) {
    let mut contexts = BTreeMap::new();
    // Per turn: the one model every turn_context named; None once they disagree or omit it.
    let mut models = BTreeMap::<String, Option<String>>::new();
    let mut current = None;
    let mut messages = Vec::new();
    let mut observations = BTreeMap::<String, Option<CodexUsageObservation>>::new();
    let mut latest_response = None;
    for line in codex_complete_records(body, recent_only) {
        let Ok(event) = oms_panel_core::decode(line, MAX_LINE) else {
            current = None;
            // An unreadable complete row can hide a conflicting response record.
            usage_session = None;
            continue;
        };
        let payload = &event["payload"];
        let turn = text(payload, "turn_id");
        match text(&event, "type") {
            "session_meta" => current = None,
            "turn_context" => {
                current = None;
                if codex_rollout_identifier(turn) {
                    let exact = codex_repo(payload, repo)
                        && payload.get("thread_id").is_none_or(|id| id == child);
                    let prior = contexts.entry(turn.to_owned()).or_insert(exact);
                    *prior &= exact;
                    let model = payload["model"]
                        .as_str()
                        .and_then(observed_model)
                        .filter(|_| exact)
                        .map(str::to_owned);
                    models
                        .entry(turn.to_owned())
                        .and_modify(|known| {
                            if *known != model {
                                *known = None;
                            }
                        })
                        .or_insert(model);
                    if *prior {
                        current = Some(turn.to_owned());
                    }
                }
            }
            "token_usage_record"
                if payload["thread_id"] == child
                    && usage_session.is_some_and(|session| payload["session_id"] == session) =>
            {
                let response_id = text(payload, "response_id");
                if codex_rollout_identifier(response_id) {
                    if let Some(known) = observations.get_mut(response_id) {
                        if known
                            .as_ref()
                            .is_some_and(|first| first.payload != *payload)
                        {
                            *known = None;
                        }
                    } else {
                        // First-seen identities define recency. Replayed records
                        // cannot restore an older response after a newer one fails.
                        latest_response = Some(response_id.to_owned());
                        observations.insert(
                            response_id.to_owned(),
                            CodexResponseUsage::parse(payload).map(|usage| CodexUsageObservation {
                                usage,
                                payload: payload.clone(),
                                model: None,
                            }),
                        );
                    }
                } else {
                    latest_response = None;
                }
            }
            "event_msg"
                if collect_chat
                    && payload["type"] == "item_completed"
                    && payload["thread_id"] == child
                    && current.as_deref() == Some(turn)
                    && payload["item"]["type"] == "AgentMessage" =>
            {
                let message = crate::rows(&payload["item"]["content"])
                    .iter()
                    .filter(|block| block["type"] == "Text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !message.trim().is_empty() {
                    messages.push((turn.to_owned(), message));
                }
            }
            _ => {}
        }
    }
    // A later contradictory context makes that turn unusable throughout the window.
    let history = messages
        .into_iter()
        .filter_map(|(turn, message)| contexts[&turn].then_some(message))
        .collect::<Vec<_>>()
        .join("\n\n");
    let observation = usage_session
        .and(latest_response)
        .and_then(|response| observations.remove(&response).flatten())
        .filter(|observation| {
            contexts
                .get(text(&observation.payload, "turn_id"))
                .is_none_or(|exact| *exact)
        })
        .map(|mut observation| {
            let turn = text(&observation.payload, "turn_id");
            observation.model = contexts
                .get(turn)
                .copied()
                .filter(|exact| *exact)
                .and_then(|_| models.remove(turn).flatten());
            observation
        });
    (history, observation)
}

/// Child chat as displayed: redacted, control-free and clipped to its last 12000 bytes.
pub(crate) fn chat_tail(history: &str) -> Option<String> {
    if history.is_empty() {
        return None;
    }
    let redacted = oms_panel_core::redact(history.as_bytes()).ok()?;
    let cleaned = redacted
        .split('\n')
        .map(|line| crate::render::clean(line, usize::MAX))
        .collect::<Vec<_>>()
        .join("\n");
    // Removing a control sequence can reconnect fragments of a secret.
    let safe = if cleaned == redacted {
        redacted
    } else {
        oms_panel_core::redact(cleaned.as_bytes()).ok()?
    };
    let safe = safe.trim();
    let start = safe.ceil_char_boundary(safe.len().saturating_sub(CODEX_CHAT_LIMIT));
    let safe = safe[start..].trim_start();
    (!safe.is_empty()).then(|| safe.to_owned())
}

#[derive(Clone, Default)]
pub(crate) enum AgyParent {
    #[default]
    Unseen,
    Legacy(PathBuf, Option<Arc<AgyLegacyPromotionMiss>>),
    // Once this pane needs a tail, it must prove custody on every observation.
    Bounded(Option<Arc<AgyRoster>>),
}

impl AgyParent {
    fn take_roster(&mut self) -> Option<Arc<AgyRoster>> {
        match self {
            Self::Bounded(projection) => projection.take(),
            _ => None,
        }
    }

    pub(crate) fn clear_roster_proof(&mut self) {
        match self {
            Self::Legacy(_, miss) => *miss = None,
            Self::Bounded(roster) => *roster = None,
            Self::Unseen => {}
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn empty_roster_watch(&self) -> Option<Arc<crate::wakeup::AgyRosterWatch>> {
        match self {
            Self::Bounded(Some(roster)) => roster.watch.clone(),
            _ => None,
        }
    }
}

pub(crate) struct AgyLegacyPromotionMiss {
    pid: u32,
    birth: String,
    repo: PathBuf,
    anchor_digest: String,
    version: AgyLegacyVersion,
    retry_after: Option<Instant>,
}

struct AgyLegacyVersion {
    path: PathBuf,
    directory: Stat,
    file: Stat,
    private_directory: bool,
}

impl AgyLegacyVersion {
    /// This stamp only suppresses repeating a failed promotion. The ordinary
    /// legacy read and strict promotion each enforce their own file checks.
    fn sample(path: PathBuf) -> Option<Self> {
        let dir = path.parent()?;
        let strict = agy_dir_strict(dir);
        let private_directory = strict.is_some();
        let (fd, directory) = strict.or_else(|| agy_dir(dir))?;
        let file = fs::statat(&fd, path.file_name()?, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        (FileType::from_raw_mode(file.st_mode) == FileType::RegularFile && file.st_nlink == 1)
            .then_some(Self {
                path,
                directory,
                file,
                private_directory,
            })
    }

    fn matches(&self, other: &Self) -> bool {
        self.path == other.path
            && self.private_directory == other.private_directory
            && codex_same_identity(&self.directory, &other.directory)
            && codex_same_version(&self.directory, &other.directory)
            && codex_same_identity(&self.file, &other.file)
            && codex_same_version(&self.file, &other.file)
    }
}

pub(crate) struct AgyRoster {
    custody: crate::agy_lead_custody::AgyLeadCustody,
    // None identifies the independently validated, markerless display mode.
    anchor_digest: Option<String>,
    directory: Stat,
    transcript: Stat,
    rows: Vec<Value>,
    #[cfg(target_os = "linux")]
    watch: Option<Arc<crate::wakeup::AgyRosterWatch>>,
}

fn agy_oversized(brain: &Path, file: &Path) -> bool {
    std::fs::symlink_metadata(brain.join(file))
        .is_ok_and(|meta| meta.is_file() && meta.len() > MAX_TRANSCRIPT as u64)
}

struct AgyTranscript {
    dir: PathBuf,
    directory: Stat,
    file: File,
    stat: Stat,
}

enum AgyRosterFailure {
    Unavailable,
    InvalidBytes(Box<Stat>),
}

impl AgyTranscript {
    fn open(custody: &crate::agy_lead_custody::AgyLeadCustody) -> Option<Self> {
        let transcript = Path::new(TRANSCRIPT);
        let dir = custody
            .app_root
            .join("brain")
            .join(&custody.conversation_id)
            .join(transcript.parent()?);
        let (opened, directory) = agy_dir_strict(&dir)?;
        let fd = fs::openat(
            &opened,
            transcript.file_name()?,
            AGY_FILE_FLAGS,
            Mode::empty(),
        )
        .ok()?;
        let stat = fs::fstat(&fd).ok()?;
        (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_nlink == 1
            && agy_strict_stat(&stat))
        .then_some(Self {
            dir,
            directory,
            file: File::from(fd),
            stat,
        })
    }

    fn rows(
        &mut self,
        anchor: Option<&str>,
        tail_limit: usize,
    ) -> Result<(Vec<Value>, usize), AgyRosterFailure> {
        let unavailable = |_| AgyRosterFailure::Unavailable;
        // The byte preceding the suffix lets a record at the exact LF boundary survive.
        let size = u64::try_from(self.stat.st_size).map_err(|_| AgyRosterFailure::Unavailable)?;
        let len = size.min(tail_limit as u64 + 1);
        self.file
            .seek(SeekFrom::Start(size - len))
            .map_err(unavailable)?;
        let mut bytes = vec![0; len as usize];
        self.file.read_exact(&mut bytes).map_err(unavailable)?;
        #[cfg(all(test, target_os = "linux"))]
        AGY_PARENT_BYTES.with(|n| n.set(n.get() + bytes.len()));
        if let Some(rows) = agy_recent_turn(&bytes, size > tail_limit as u64, anchor) {
            return Ok(rows);
        }
        let after = fs::fstat(&self.file).map_err(|_| AgyRosterFailure::Unavailable)?;
        if codex_same_identity(&self.stat, &after) && codex_same_version(&self.stat, &after) {
            Err(AgyRosterFailure::InvalidBytes(Box::new(self.stat)))
        } else {
            Err(AgyRosterFailure::Unavailable)
        }
    }

    fn confirm(&self, custody: &crate::agy_lead_custody::AgyLeadCustody) -> bool {
        note_legacy_custody_call(1);
        if crate::agy_lead_custody::agy_lead_custody(custody.pid, &custody.birth, &custody.repo)
            .as_ref()
            != Some(custody)
        {
            return false;
        }
        (|| {
            let (reopened, directory) = agy_dir_strict(&self.dir)?;
            let after = fs::fstat(&self.file).ok()?;
            let name = Path::new(TRANSCRIPT).file_name()?;
            let named = fs::statat(&reopened, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
            Some(
                codex_same_identity(&self.directory, &directory)
                    && codex_same_identity(&self.stat, &after)
                    && codex_same_version(&self.stat, &after)
                    && codex_same_identity(&self.stat, &named)
                    && codex_same_version(&self.stat, &named),
            )
        })() == Some(true)
    }
}

fn agy_custody_rows(
    custody: &crate::agy_lead_custody::AgyLeadCustody,
    anchor: Option<&str>,
    tail_limit: usize,
) -> Option<(Vec<Value>, usize)> {
    let mut transcript = AgyTranscript::open(custody)?;
    let rows = transcript.rows(anchor, tail_limit).ok()?;
    transcript.confirm(custody).then_some(rows)
}

pub(crate) fn agy_submitted_user_turn(
    pid: u32,
    birth: &str,
    repo: &Path,
    turn: &str,
    expected: &str,
) -> bool {
    const SEND_TAIL_LIMIT: usize = 64 * 1024;
    crate::agy_lead_custody::agy_lead_custody(pid, birth, repo)
        .and_then(|custody| agy_custody_rows(&custody, None, SEND_TAIL_LIMIT))
        .is_some_and(|(rows, start)| {
            // agy_recent_turn first validates the latest explicit user row and its wrapper.
            agy_user_request(&rows[start - 1]).is_some_and(|(body, _)| {
                body == expected && body.lines().next().and_then(codex_turn) == Some(turn)
            })
        })
}

fn agy_user_request(row: &Value) -> Option<(&str, &str)> {
    text(row, "content")
        .strip_prefix("<USER_REQUEST>\n")?
        .split_once("\n</USER_REQUEST>")
}

fn agy_display_request_suffix(suffix: &str) -> bool {
    if suffix.trim().is_empty() {
        return true;
    }
    // The native clock stamp is framing evidence only.
    suffix
        .strip_prefix("\n<ADDITIONAL_METADATA>\nThe current local time is: ")
        .and_then(|time| time.strip_suffix(".\n</ADDITIONAL_METADATA>"))
        .is_some_and(|time| {
            (20..=35).contains(&time.len())
                && matches!(time.as_bytes().get(10), Some(b'T' | b't'))
                && time.bytes().all(|b| b.is_ascii_graphic())
                && chrono::DateTime::parse_from_rfc3339(time).is_ok()
        })
}

fn agy_recent_turn(
    bytes: &[u8],
    recent: bool,
    anchor: Option<&str>,
) -> Option<(Vec<Value>, usize)> {
    let derived = anchor.is_none();
    let start = if recent {
        bytes.iter().position(|&b| b == b'\n')? + 1
    } else {
        0
    };
    let complete = bytes.get(start..)?.strip_suffix(b"\n")?;
    let mut rows = Vec::new();
    for line in complete.split(|&b| b == b'\n') {
        if rows.len() >= crate::agy_child_records::LEAD_ROW_BUDGET {
            return None;
        }
        let row = oms_panel_core::decode(line, MAX_LINE).ok()?;
        if derived
            && (line.contains(&b'\r')
                || ["source", "type", "status"]
                    .into_iter()
                    .any(|key| text(&row, key).is_empty()))
        {
            return None;
        }
        rows.push(row);
    }
    let user = |row: &Value| row["type"] == "USER_INPUT" || row["source"] == "USER_EXPLICIT";
    let current = rows.iter().rposition(user)?;
    let input = &rows[current];
    let (body, suffix) = agy_user_request(input)?;
    let first = body.lines().next()?;
    let turn = codex_turn(first)?;
    if input["type"] != "USER_INPUT"
        || input["source"] != "USER_EXPLICIT"
        || input["status"] != "DONE"
        || input.get("truncated_fields").is_some()
        || body.contains("<USER_REQUEST>")
        || suffix.contains("<USER_REQUEST>")
        || body[TASK_PREFIX.len()..].contains(TASK_PREFIX)
        || suffix.contains(TASK_PREFIX)
        || anchor.is_some_and(|anchor| !first.starts_with(anchor))
        || (derived && (!agy_display_request_suffix(suffix) || body.contains("<CONTEXT_SUMMARY>")))
    {
        return None;
    }
    let mut turns = BTreeSet::new();
    for prior in rows.iter().filter(|row| user(row)) {
        for observed in text(prior, "content").lines().filter_map(codex_turn) {
            if (derived || observed == turn) && !turns.insert(observed) {
                return None;
            }
        }
    }
    let contiguous = if derived { &rows[..] } else { &rows[current..] };
    let origin = contiguous.first()?["step_index"].as_u64()?;
    if (derived && !recent && origin != 0)
        || contiguous.windows(2).any(|pair| {
            pair[0]["step_index"]
                .as_u64()
                .and_then(|step| step.checked_add(1))
                != pair[1]["step_index"].as_u64()
                || !pair[1]["step_index"].is_u64()
        })
    {
        return None;
    }
    Some((rows, current + 1))
}

/// A successful capture may lack history, but a conflicting visible task is not absence.
fn agy_display_anchor(terminal: &str) -> Option<Option<&str>> {
    const AGY_HINT: &str = "Auto mode: edits & safe commands auto-approved (shift+tab to cycle)";
    if terminal.trim().is_empty() || terminal.chars().any(|c| c.is_control() && c != '\n') {
        return None;
    }
    let mut prompts = terminal.lines().rev().map(str::trim).filter(|line| {
        line.starts_with('>') && *line != ">" && line.strip_prefix("> ") != Some(AGY_HINT)
    });
    let Some(latest) = prompts.next() else {
        return Some(None);
    };
    let prompt = latest.strip_prefix("> ")?;
    let turn = codex_turn(prompt)?;
    if prompts.any(|prior| codex_turn(prior[1..].trim_start()) == Some(turn))
        || prompt[TASK_PREFIX.len()..].contains(TASK_PREFIX)
    {
        return None;
    }
    let end = prompt
        .char_indices()
        .nth(200)
        .map_or(prompt.len(), |(i, _)| i);
    Some(Some(&prompt[..end]))
}

fn agy(
    pid: u32,
    birth: &str,
    repo: &Path,
    terminal: &str,
    parent: &mut AgyParent,
    reuse_roster: bool,
    now: Instant,
) -> Option<Vec<Value>> {
    let previous = parent.take_roster();
    let Some(anchor) = agy_display_anchor(terminal) else {
        parent.clear_roster_proof();
        return None;
    };
    if anchor.is_none()
        && let AgyParent::Legacy(_, miss) = parent
    {
        *miss = None;
    }
    let matches_prompt = |rows: &[Value], anchor: &str| {
        rows.iter().any(|row| {
            row["type"] == "USER_INPUT"
                && row["source"] == "USER_EXPLICIT"
                && text(row, "content")
                    .lines()
                    .any(|line| line.starts_with(anchor))
        })
    };
    let home = anchor
        .and_then(|_| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".gemini/antigravity-cli/brain"));

    // 1. Warm cache: direct read of the cached parent transcript without custody or read_dir.
    if matches!(parent, AgyParent::Legacy(file, _)
        if home.as_ref().is_some_and(|brain| agy_oversized(brain, file)))
    {
        *parent = AgyParent::Bounded(None);
    }
    let cached_rows = (|| {
        let anchor = anchor?;
        let AgyParent::Legacy(file, _) = &*parent else {
            return None;
        };
        let store = Store::open(home.as_ref()?).ok()?;
        let rows = jsonl(&store, file)?;
        if matches_prompt(&rows, anchor) {
            note_legacy_parent_read(1);
            Some((rows, store.repo().to_path_buf()))
        } else {
            note_legacy_candidate_read(1);
            None
        }
    })();
    if anchor.is_some() && cached_rows.is_none() && matches!(parent, AgyParent::Legacy(..)) {
        *parent = AgyParent::Unseen;
    }

    let warm_legacy = cached_rows.is_some();
    let mut small_custody = None;
    let (rows, legacy_root) = if let Some(cached) = cached_rows {
        cached
    } else if let Some(custody) = {
        note_legacy_custody_call(1);
        crate::agy_lead_custody::agy_lead_custody(pid, birth, repo)
    } {
        // 2. Optimized custody-proved cold path: direct read of custody.conversation_id only.
        let brain = custody.app_root.join("brain");
        let file = PathBuf::from(&custody.conversation_id).join(TRANSCRIPT);
        if anchor.is_none()
            || matches!(parent, AgyParent::Bounded(_))
            || agy_oversized(&brain, &file)
        {
            if anchor.is_some() || reuse_roster {
                *parent = AgyParent::Bounded(None);
            }
            let cache = match parent {
                AgyParent::Bounded(cache) if reuse_roster => Some(cache),
                _ => None,
            };
            return agy_bounded_roster(&custody, anchor, cache, previous).ok();
        } else {
            let store = Store::open(&brain).ok()?;
            let rows = jsonl(&store, &file)?;
            if !matches_prompt(&rows, anchor?) {
                note_legacy_candidate_read(1);
                return None;
            }
            note_legacy_parent_read(1);
            *parent = AgyParent::Legacy(file, None);
            small_custody = Some(custody);
            (rows, store.repo().to_path_buf())
        }
    } else {
        // 3. Legacy fallback cold path only when custody is unavailable.
        if matches!(parent, AgyParent::Bounded(_)) {
            return None;
        }
        let anchor = anchor?;
        let brain = home?;
        let store = Store::open(&brain).ok()?;
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&brain).ok()?.take(2048).flatten() {
            let file = entry.file_name();
            if !file.to_str().is_some_and(uuid) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let source = Path::new(&file).join(TRANSCRIPT);
            if let Ok(meta) = std::fs::symlink_metadata(brain.join(&source))
                && meta.is_file()
                && meta.len() <= MAX_TRANSCRIPT as u64
                && let Ok(when) = meta.modified()
            {
                candidates.push((when, source));
            }
        }
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        let mut selected = None;
        let mut scanned = 0usize;
        for (_, path) in candidates.into_iter().take(24) {
            let Some(rows) = jsonl(&store, &path) else {
                continue;
            };
            scanned += 1;
            if matches_prompt(&rows, anchor) && selected.replace((rows, path)).is_some() {
                note_legacy_candidate_read(scanned);
                return None;
            }
        }
        let Some((rows, file)) = selected else {
            note_legacy_candidate_read(scanned);
            return None;
        };
        note_legacy_parent_read(1);
        note_legacy_candidate_read(scanned.saturating_sub(1));
        *parent = AgyParent::Legacy(file, None);
        (rows, store.repo().to_path_buf())
    };

    let roster = agy_roster_rows(&rows, &mut AgyWorkspaces::new(repo), None);
    let AgyParent::Legacy(file, miss) = parent else {
        return Some(roster);
    };
    if !reuse_roster || !roster.is_empty() || (!warm_legacy && small_custody.is_none()) {
        *miss = None;
        return Some(roster);
    }

    let anchor_digest = oms_panel_core::digest(anchor?.as_bytes());
    let version = AgyLegacyVersion::sample(legacy_root.join(file));
    if let Some(miss) = miss.as_ref()
        && miss.pid == pid
        && miss.birth == birth
        && miss.repo == repo
        && miss.anchor_digest == anchor_digest
        && miss.retry_after.is_none_or(|deadline| now < deadline)
        && version
            .as_ref()
            .is_some_and(|version| miss.version.matches(version))
    {
        return Some(roster);
    }
    *miss = None;

    let custody = small_custody.or_else(|| {
        note_legacy_custody_call(1);
        crate::agy_lead_custody::agy_lead_custody(pid, birth, repo)
    });
    let same_source = custody.as_ref().is_none_or(|custody| {
        version.as_ref().is_some_and(|version| {
            custody
                .app_root
                .join("brain")
                .join(&custody.conversation_id)
                .join(TRANSCRIPT)
                == version.path
        })
    });
    // An empty anchored roster can wake only after the same strict
    // descriptor and current-Turn proof as the bounded Board path.
    // Unavailable custody can recover without a transcript write.
    let mut retry_after = Some(now + crate::watch::EVENT_BACKED_FALLBACK);
    if let Some(custody) = custody.as_ref() {
        let mut cache = None;
        match agy_bounded_roster(custody, anchor, Some(&mut cache), None) {
            Ok(verified) => {
                if let Some(cache) = cache {
                    *parent = AgyParent::Bounded(Some(cache));
                    return Some(verified);
                }
                if !verified.is_empty() {
                    return Some(verified);
                }
            }
            Err(AgyRosterFailure::InvalidBytes(rejected))
                if version.as_ref().is_some_and(|version| {
                    codex_same_identity(&version.file, &rejected)
                        && codex_same_version(&version.file, &rejected)
                }) =>
            {
                retry_after = None;
            }
            Err(_) => {}
        }
    }
    if same_source
        && let Some(version) = version
        && AgyLegacyVersion::sample(version.path.clone())
            .is_some_and(|after| version.matches(&after))
    {
        *miss = Some(Arc::new(AgyLegacyPromotionMiss {
            pid,
            birth: birth.to_owned(),
            repo: repo.to_path_buf(),
            anchor_digest,
            version,
            retry_after,
        }));
    }
    Some(roster)
}

fn agy_bounded_roster(
    custody: &crate::agy_lead_custody::AgyLeadCustody,
    anchor: Option<&str>,
    mut cache: Option<&mut Option<Arc<AgyRoster>>>,
    previous: Option<Arc<AgyRoster>>,
) -> Result<Vec<Value>, AgyRosterFailure> {
    let mut transcript = AgyTranscript::open(custody).ok_or(AgyRosterFailure::Unavailable)?;
    let anchor_digest = anchor.map(|anchor| oms_panel_core::digest(anchor.as_bytes()));
    if let Some(cache) = cache.as_deref_mut()
        && let Some(previous) = previous.as_ref()
        && previous.custody == *custody
        && previous.anchor_digest == anchor_digest
        && codex_same_identity(&previous.directory, &transcript.directory)
        && codex_same_identity(&previous.transcript, &transcript.stat)
        && codex_same_version(&previous.transcript, &transcript.stat)
    {
        let rows = previous.rows.clone();
        if !transcript.confirm(custody) {
            return Err(AgyRosterFailure::Unavailable);
        }
        *cache = Some(previous.clone());
        return Ok(rows);
    }
    let (rows, start) = transcript.rows(anchor, MAX_TRANSCRIPT)?;
    let scope = AgyDisplayScope {
        first_spawn: start,
        custody,
        derived: anchor.is_none(),
    };
    let mut workspaces = AgyWorkspaces::new(&custody.repo);
    let rows = agy_roster_rows(&rows, &mut workspaces, Some(scope));
    let cache = cache.filter(|_| {
        workspaces.verdicts.is_empty()
            && rows.iter().all(|row| {
                ["updated_at", "created_at", "started_at", "ended_at"]
                    .into_iter()
                    .all(|field| {
                        row.get(field).is_none_or(|value| {
                            value.as_str().is_some_and(|stamp| {
                                stamp.is_empty()
                                    || (stamp.len() <= 35
                                        && chrono::DateTime::parse_from_rfc3339(stamp).is_ok())
                            })
                        })
                    })
            })
    });
    #[cfg(target_os = "linux")]
    let watch = cache.as_ref().filter(|_| rows.is_empty()).and_then(|_| {
        previous
            .as_deref()
            .filter(|previous| {
                previous.custody == *custody
                    && previous.anchor_digest == anchor_digest
                    && codex_same_identity(&previous.directory, &transcript.directory)
                    && codex_same_identity(&previous.transcript, &transcript.stat)
            })
            .and_then(|previous| previous.watch.as_ref())
            .filter(|watch| watch.active())
            .cloned()
            .or_else(|| crate::wakeup::AgyRosterWatch::arm(&transcript.file))
    });
    if !transcript.confirm(custody) {
        return Err(AgyRosterFailure::Unavailable);
    }
    note_legacy_parent_read(1);
    if let Some(cache) = cache {
        *cache = Some(Arc::new(AgyRoster {
            custody: custody.clone(),
            anchor_digest,
            directory: transcript.directory,
            transcript: transcript.stat,
            rows: rows.clone(),
            #[cfg(target_os = "linux")]
            watch,
        }));
    }
    Ok(rows)
}

fn agy_roster_rows(
    rows: &[Value],
    workspaces: &mut AgyWorkspaces,
    scope: Option<AgyDisplayScope<'_>>,
) -> Vec<Value> {
    agy_children(rows, workspaces, scope)
        .into_iter()
        .rev()
        // The Board can show the bounded roster. Usage samples only its first three.
        .take(crate::agy_child_records::SPAWN_BUDGET)
        .map(
            |AgyProjectedChild {
                 id,
                 title,
                 model,
                 started,
                 spawn_result_idx,
                 recovered,
             }| {
                if recovered {
                    // A parent delivery proves a message, which can precede
                    // termination. The recovered identity proves no lifecycle.
                    return row("antigravity", &id, &title, &model, started, "", "unknown");
                }
                let finished = rows[spawn_result_idx + 1..]
                    .iter()
                    .rev()
                    .filter(|msg| {
                        msg["type"] == "SYSTEM_MESSAGE"
                            && msg["source"] == "SYSTEM"
                            && msg["status"] == "DONE"
                            && (scope.is_none() || msg.get("truncated_fields").is_none())
                    })
                    .find_map(|msg| {
                        agy_delivery(text(msg, "content"), &id).map(|body| (msg, body))
                    });
                let (state, updated, answer) = match finished {
                    Some((msg, body)) => (
                        "done",
                        Some(text(msg, "created_at"))
                            .filter(|s| !s.is_empty())
                            .unwrap_or(started),
                        body,
                    ),
                    None => ("unknown", started, ""),
                };
                let mut call = row("antigravity", &id, &title, &model, updated, answer, state);
                if let Some((msg, _)) = finished {
                    call["started_at"] = json!(started);
                    call["ended_at"] = json!(text(msg, "created_at"));
                }
                call
            },
        )
        .collect()
}

fn agy_child_model(event: &Value, tool: &Value) -> String {
    fn pick(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|s| {
            !s.is_empty()
                && !s.eq_ignore_ascii_case("inherit")
                && !s.eq_ignore_ascii_case("default")
        })
    }
    pick(event["model"].as_str())
        .or_else(|| pick(event["spec"]["model"].as_str()))
        .or_else(|| pick(tool["args"]["Model"].as_str()))
        .or_else(|| pick(tool["args"]["model"].as_str()))
        .unwrap_or("unknown")
        .to_owned()
}

struct AgyProjectedChild<'a> {
    id: String,
    title: String,
    model: String,
    started: &'a str,
    spawn_result_idx: usize,
    recovered: bool,
}

enum AgyChildWorkspaces {
    Legacy { root: bool, uris: Vec<String> },
    RecoveredBatch(usize),
}

const AGY_CREATED: &str = "Created the following subagent:\n";
const AGY_CREATED_FOOTER: &str = "\nThe subagent will send you a message when it has completed its task or requires guidance. There is no need to poll for its response.";

fn agy_recovered_result(
    content: &str,
    custody: &crate::agy_lead_custody::AgyLeadCustody,
) -> Option<(String, Vec<String>)> {
    let mut body = content;
    let mut stamps = 0;
    for label in ["Created At: ", "Completed At: "] {
        if let Some(header) = body.strip_prefix(label) {
            let (stamp, rest) = header.split_once('\n')?;
            chrono::DateTime::parse_from_rfc3339(stamp).ok()?;
            body = rest;
            stamps += 1;
        }
    }
    // AGY can repeat the exact validated pair before the current long envelope.
    let repeated = if stamps == 2 {
        body.strip_prefix(&content[..content.len() - body.len()])
    } else {
        None
    };
    let body = repeated
        .unwrap_or(body)
        .strip_prefix(AGY_CREATED)?
        .trim_end();
    let body = if repeated.is_some() {
        body.strip_suffix(AGY_CREATED_FOOTER)?
    } else {
        body.strip_suffix("\nThe subagent will send you a message when done.")
            .unwrap_or(body)
    };
    let event = oms_panel_core::decode(body.as_bytes(), MAX_LINE).ok()?;
    let id = event["conversationId"].as_str()?;
    if !uuid(id)
        || id == custody.conversation_id
        || id.bytes().any(|b| b.is_ascii_uppercase())
        || event
            .get("parentConversationId")
            .filter(|parent| !parent.is_null())
            .is_some_and(|parent| parent != &custody.conversation_id)
    {
        return None;
    }
    if let Some(log) = event.get("logAbsoluteUri").filter(|v| !v.is_null()) {
        let path = custody.app_root.join("brain").join(id).join(TRANSCRIPT);
        if log.as_str()? != format!("file://{}", path.to_str()?) {
            return None;
        }
    }
    let uris = event["workspaceUris"].as_array()?;
    if !(1..=AGY_URIS_PER_RECORD).contains(&uris.len()) {
        return None;
    }
    let uris = uris
        .iter()
        .map(|value| {
            let uri = value.as_str()?;
            let path = uri.strip_prefix("file:///")?;
            (!path.contains(['%', '?', '#', '\\'])
                && !path.chars().any(char::is_control)
                && !path.split('/').any(|part| matches!(part, "" | "." | "..")))
            .then(|| uri.to_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    Some((id.to_owned(), uris))
}

fn agy_recovered_batch<'a>(
    rows: &'a [Value],
    index: usize,
    custody: &crate::agy_lead_custody::AgyLeadCustody,
) -> Option<(Vec<AgyProjectedChild<'a>>, BTreeSet<String>)> {
    let planner = &rows[index];
    let step = planner["step_index"].as_u64()?;
    let calls = planner["tool_calls"].as_array()?;
    if planner["status"] != "DONE"
        || !(1..=crate::agy_child_records::TOOL_BATCH_LIMIT).contains(&calls.len())
    {
        return None;
    }
    let end = index + 1 + calls.len();
    let results = rows.get(index + 1..end)?;
    if rows.get(end).is_some_and(|row| row["type"] == "GENERIC") {
        return None;
    }
    let mut recovered = Vec::new();
    let mut tool_ids = BTreeSet::new();
    let mut workspaces = BTreeSet::new();
    for (offset, (slot, result)) in calls.iter().zip(results).enumerate() {
        // The surviving name is only a consistency check. Ignore all arguments
        // and derive the identity and workspace evidence from the result object.
        let name = slot["name"].as_str().filter(|name| !name.is_empty())?;
        if let Some(id) = slot.get("id").filter(|id| !id.is_null())
            && !tool_ids.insert(id.as_str().filter(|id| !id.is_empty())?)
        {
            return None;
        }
        let content = result["content"].as_str()?;
        let status = result["status"].as_str()?;
        if result["source"] != "MODEL"
            || result["type"] != "GENERIC"
            || result["step_index"].as_u64()? != step.checked_add(offset as u64 + 1)?
            || !matches!(status, "DONE" | "ERROR")
            || result.get("truncated_fields").is_some()
        {
            return None;
        }
        if name != "invoke_subagent" {
            if content.contains(AGY_CREATED) {
                return None;
            }
            continue;
        }
        if status != "DONE" {
            return None;
        }
        let (id, uris) = agy_recovered_result(content, custody)?;
        workspaces.extend(uris);
        recovered.push(AgyProjectedChild {
            id,
            title: "Native subagent".into(),
            model: "unknown".into(),
            started: text(result, "created_at"),
            spawn_result_idx: index + 1 + offset,
            recovered: true,
        });
    }
    Some((recovered, workspaces))
}

fn agy_child_key(id: &str, bounded: bool) -> std::borrow::Cow<'_, str> {
    if bounded && id.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(id.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(id)
    }
}

#[derive(Clone, Copy)]
struct AgyDisplayScope<'a> {
    first_spawn: usize,
    custody: &'a crate::agy_lead_custody::AgyLeadCustody,
    derived: bool,
}

fn agy_children<'a>(
    rows: &'a [Value],
    workspaces: &mut AgyWorkspaces,
    scope: Option<AgyDisplayScope<'_>>,
) -> Vec<AgyProjectedChild<'a>> {
    let first_spawn = scope.map_or(0, |scope| scope.first_spawn);
    let mut records = Vec::new();
    let mut batch_workspaces = Vec::new();
    for (index, planner) in rows.iter().enumerate() {
        if planner["source"] != "MODEL" || planner["type"] != "PLANNER_RESPONSE" {
            continue;
        }
        let Some(step) = planner["step_index"].as_u64() else {
            continue;
        };
        let clipped_calls = planner["truncated_fields"]
            .as_array()
            .is_some_and(|fields| matches!(fields.as_slice(), [field] if field == "tool_calls"));
        if let Some(scope) = scope
            && (scope.derived || clipped_calls)
        {
            if (clipped_calls || planner.get("truncated_fields").is_none())
                && let Some((batch, uris)) = agy_recovered_batch(rows, index, scope.custody)
            {
                let batch_id = batch_workspaces.len();
                batch_workspaces.push(uris);
                records.extend(
                    batch
                        .into_iter()
                        .map(|child| (child, AgyChildWorkspaces::RecoveredBatch(batch_id), true)),
                );
            }
            continue;
        }
        let calls = crate::rows(&planner["tool_calls"]);
        let multiple = calls
            .iter()
            .filter(|tool| tool["name"] == "invoke_subagent")
            .take(2)
            .count()
            > 1;
        // All calls have a result slot, including calls unrelated to child creation.
        for (offset, (tool, entry)) in calls.iter().zip(&rows[index + 1..]).enumerate() {
            let Some(expected) = step.checked_add(offset as u64 + 1) else {
                break;
            };
            if entry["step_index"].as_u64() != Some(expected)
                || entry["source"] != "MODEL"
                || entry["type"] != "GENERIC"
                || entry["status"] != "DONE"
            {
                break;
            }
            if tool["name"] != "invoke_subagent" {
                continue;
            }
            let Some((_, body)) = text(entry, "content").split_once(AGY_CREATED) else {
                continue;
            };
            let Some((body, _)) = body.split_once("\n}") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(&format!("{body}\n}}")) else {
                continue;
            };
            let id = text(&event, "conversationId");
            if !uuid(id) {
                continue;
            }
            let all = crate::rows(&event["workspaceUris"]);
            let root = all.iter().any(|uri| uri == workspaces.root.as_str());
            // Only on-disk candidates are capped; the root needs no filesystem work.
            let uris: Vec<String> = all
                .iter()
                .filter_map(Value::as_str)
                .filter(|uri| *uri != workspaces.root)
                .take(AGY_URIS_PER_RECORD)
                .map(str::to_owned)
                .collect();
            let title = if multiple {
                "Native subagent".to_owned()
            } else {
                agy_title(tool)
            };
            let model = agy_child_model(&event, tool);
            let spawn = index + 1 + offset;
            let complete = first_spawn == 0
                || (planner.get("truncated_fields").is_none()
                    && entry.get("truncated_fields").is_none());
            records.push((
                AgyProjectedChild {
                    id: id.to_owned(),
                    title,
                    model,
                    started: text(entry, "created_at"),
                    spawn_result_idx: spawn,
                    recovered: false,
                },
                AgyChildWorkspaces::Legacy { root, uris },
                complete,
            ));
        }
    }
    let mut created = BTreeMap::new();
    for (child, ..) in &records {
        *created
            .entry(agy_child_key(&child.id, scope.is_some()))
            .or_insert(0usize) += 1;
    }
    let mut batches = BTreeMap::new();
    for (child, evidence, _) in &records {
        if let AgyChildWorkspaces::RecoveredBatch(batch) = evidence
            && created[&agy_child_key(&child.id, scope.is_some())] != 1
        {
            batches.insert(*batch, false);
        }
    }
    // One child id from two creation records cannot be attributed to either; the
    // newest records, which the panel shows, spend the frame's check budget first.
    let keep: Vec<bool> = records
        .iter()
        .rev()
        .map(|(child, evidence, complete)| {
            *complete
                && child.spawn_result_idx >= first_spawn
                && created[&agy_child_key(&child.id, scope.is_some())] == 1
                && match evidence {
                    AgyChildWorkspaces::Legacy { root, uris } => {
                        *root || uris.iter().any(|uri| workspaces.accepts(uri))
                    }
                    AgyChildWorkspaces::RecoveredBatch(batch) => {
                        *batches.entry(*batch).or_insert_with(|| {
                            batch_workspaces[*batch]
                                .iter()
                                .all(|uri| workspaces.accepts(uri))
                        })
                    }
                }
        })
        .collect();
    records
        .into_iter()
        .zip(keep.into_iter().rev())
        .filter_map(|((child, ..), keep)| keep.then_some(child))
        .collect()
}

/// Workspaces whose AGY children belong to `repo`: its root, or a private linked
/// worktree registered in, and pointing back to, the same Git common directory.
///
/// A verdict is display and discovery evidence only. It must never authorize a
/// chat, a write or any other action on the worktree: the same-group rule in
/// `agy_private` admits a tree that other members of a shared group could edit.
struct AgyWorkspaces {
    root: String,
    repo: PathBuf,
    common: Option<Option<(OwnedFd, Stat)>>,
    verdicts: BTreeMap<String, bool>,
    bytes: usize,
}

impl AgyWorkspaces {
    fn new(repo: &Path) -> Self {
        Self {
            root: format!("file://{}", repo.display()),
            repo: repo.to_owned(),
            common: None,
            verdicts: BTreeMap::new(),
            bytes: AGY_META_BUDGET,
        }
    }

    fn accepts(&mut self, uri: &str) -> bool {
        if uri == self.root {
            return true;
        }
        if let Some(&verdict) = self.verdicts.get(uri) {
            return verdict;
        }
        if self.verdicts.len() >= AGY_URIS_PER_FRAME {
            return false;
        }
        let (repo, bytes) = (&self.repo, &mut self.bytes);
        let verdict = self
            .common
            .get_or_insert_with(|| {
                agy_common(repo, bytes).filter(|(_, stat)| agy_private(stat, stat))
            })
            .as_ref()
            .is_some_and(|common| agy_linked(common, uri, &mut self.bytes).is_some());
        self.verdicts.insert(uri.to_owned(), verdict);
        verdict
    }
}

fn agy_same(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

/// Opens an absolute, already-normal path without following any symlink.
fn agy_dir(path: &Path) -> Option<(OwnedFd, Stat)> {
    let rest = path.to_str()?.strip_prefix('/')?;
    if rest
        .split('/')
        .any(|name| name.is_empty() || name == "." || name == "..")
    {
        return None;
    }
    let root = fs::open("/", CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    let fd = codex_directory(&root, Path::new(rest))?;
    let stat = fs::fstat(&fd).ok()?;
    Some((fd, stat))
}

/// Resolves a Git metadata path lexically; `agy_dir` then refuses symlinks.
fn agy_join(base: &Path, target: &str) -> Option<PathBuf> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.to_str()?
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    };
    for name in target.split('/') {
        match name {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    Some(PathBuf::from(format!("/{}", parts.join("/"))))
}

/// Owner and write bits an admitted worktree object may carry. Group write passes
/// only when the canonical common directory shares the group and is itself
/// group-writable, so the object is no more exposed than the repository; this is
/// discovery evidence, never authority (see `AgyWorkspaces`).
fn agy_private(stat: &Stat, common: &Stat) -> bool {
    stat.st_uid == rustix::process::geteuid().as_raw()
        && stat.st_mode & 0o002 == 0
        && (stat.st_mode & 0o020 == 0
            || (stat.st_gid == common.st_gid && common.st_mode & 0o020 != 0))
}

/// One bounded, single-line pointer file read without following symlinks; the
/// caller checks the returned stat with `agy_private`.
fn agy_meta(dir: &OwnedFd, name: &str, bytes: &mut usize) -> Option<(String, Stat)> {
    agy_meta_checked(dir, name, bytes, |_, _| true)
}

fn agy_meta_checked(
    dir: &OwnedFd,
    name: &str,
    bytes: &mut usize,
    admissible: impl FnOnce(&OwnedFd, &Stat) -> bool,
) -> Option<(String, Stat)> {
    let fd = fs::openat(dir, name, AGY_FILE_FLAGS, Mode::empty()).ok()?;
    let before = fs::fstat(&fd).ok()?;
    let size = usize::try_from(before.st_size)
        .ok()
        .filter(|&size| size <= AGY_META_LIMIT)?;
    // The probe includes one byte past the size and is spent even if it fails.
    *bytes = bytes.checked_sub(size + 1)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_nlink != 1
        || !admissible(&fd, &before)
    {
        return None;
    }
    let file = File::from(fd);
    let mut buf = Vec::with_capacity(size);
    (&file).take(size as u64 + 1).read_to_end(&mut buf).ok()?;
    let after = fs::fstat(&file).ok()?;
    if buf.len() != size
        || !codex_same_identity(&before, &after)
        || !codex_same_version(&before, &after)
    {
        return None;
    }
    let text = String::from_utf8(buf).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(&text);
    (!line.is_empty() && !line.contains(['\n', '\r', '\0'])).then(|| (line.to_owned(), before))
}

/// The common directory of `repo`, whether it is a main or a linked checkout.
fn agy_common(repo: &Path, bytes: &mut usize) -> Option<(OwnedFd, Stat)> {
    let repo = &repo.canonicalize().ok()?;
    let (repo_fd, _) = agy_dir(repo)?;
    let dot = fs::statat(&repo_fd, ".git", AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(dot.st_mode) == FileType::Directory {
        return agy_dir(&repo.join(".git")).filter(|(_, stat)| agy_same(stat, &dot));
    }
    let (pointer, pointer_stat) = agy_meta(&repo_fd, ".git", bytes)?;
    let admin = agy_join(repo, pointer.strip_prefix("gitdir: ")?)?;
    let (admin_fd, admin_stat) = agy_dir(&admin)?;
    let mut seen = vec![pointer_stat, admin_stat];
    let common = match fs::statat(&admin_fd, "commondir", AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => (admin_fd, admin_stat),
        Err(_) => return None,
        Ok(_) => {
            let (shared, stat) = agy_meta(&admin_fd, "commondir", bytes)?;
            seen.push(stat);
            agy_dir(&agy_join(&admin, &shared)?)?
        }
    };
    seen.iter()
        .all(|stat| agy_private(stat, &common.1))
        .then_some(common)
}

/// Proves that `uri` names exactly a live linked worktree of `common`; every
/// unverifiable, symlinked, foreign, pruned or replaced record is refused.
fn agy_linked(
    common: &(OwnedFd, Stat),
    uri: &str,
    bytes: &mut usize,
) -> Option<(OwnedFd, PathBuf)> {
    let path = uri.strip_prefix("file://")?;
    // AGY sends plain absolute paths; encoded or decorated URIs stay ambiguous.
    if path.contains(['%', '?', '#', '\\']) || path.chars().any(char::is_control) {
        return None;
    }
    let tree = Path::new(path);
    let (tree_fd, tree_stat) = agy_dir(tree)?;
    let private = |stat: &Stat| agy_private(stat, &common.1);
    if !private(&tree_stat) {
        return None;
    }
    let (pointer, pointer_stat) = agy_meta(&tree_fd, ".git", bytes)?;
    let admin = agy_join(tree, pointer.strip_prefix("gitdir: ")?)?;
    let name = admin.file_name()?.to_str()?;
    let worktrees = admin.parent()?;
    if worktrees.file_name()? != "worktrees"
        || !agy_same(&agy_dir(worktrees.parent()?)?.1, &common.1)
    {
        return None;
    }
    let registry = fs::openat(&common.0, "worktrees", CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    let record = fs::openat(&registry, name, CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    let (backlink, backlink_stat) = agy_meta(&record, "gitdir", bytes)?;
    let (shared, shared_stat) = agy_meta(&record, "commondir", bytes)?;
    let (registry_stat, record_stat) = (fs::fstat(&registry).ok()?, fs::fstat(&record).ok()?);
    let admin_stat = agy_dir(&admin)?.1;
    let (backlink, shared) = (agy_join(&admin, &backlink)?, agy_join(&admin, &shared)?);
    ([
        pointer_stat,
        registry_stat,
        record_stat,
        admin_stat,
        backlink_stat,
        shared_stat,
    ]
    .iter()
    .all(private)
        && agy_same(&record_stat, &admin_stat)
        && backlink == tree.join(".git")
        && agy_same(&agy_dir(&shared)?.1, &common.1)
        && agy_same(&agy_dir(tree)?.1, &tree_stat))
    .then_some((record, admin))
}

/// Display-only observation of HEAD in the proved Main launch checkout.
pub(crate) fn observe_checkout(repo: &Path, checkout: &Path) -> Option<(String, Option<String>)> {
    let mut bytes = AGY_META_BUDGET;
    let common = agy_common(repo, &mut bytes)?;
    if !agy_private(&common.1, &common.1) {
        return None;
    }
    let open = |bytes: &mut usize| {
        if checkout == repo
            && let Some((dir, stat)) = agy_dir(&checkout.join(".git"))
            && agy_same(&stat, &common.1)
        {
            return Some((dir, None));
        }
        let (dir, admin) = agy_linked(&common, &format!("file://{}", checkout.to_str()?), bytes)?;
        Some((dir, Some(admin)))
    };
    let (dir, admin) = open(&mut bytes)?;
    let (head, before) = agy_meta(&dir, "HEAD", &mut bytes)?;
    let checked_common = agy_common(repo, &mut bytes)?;
    let (check, checked_admin) = open(&mut bytes)?;
    let after = fs::statat(&check, "HEAD", AtFlags::SYMLINK_NOFOLLOW).ok()?;
    let label = match &admin {
        Some(path) => path.file_name()?.to_str()?,
        None => "repository",
    };
    if admin != checked_admin
        || !agy_same(&common.1, &checked_common.1)
        || !agy_private(&checked_common.1, &checked_common.1)
        || !agy_same(&fs::fstat(&dir).ok()?, &fs::fstat(&check).ok()?)
        || !agy_private(&before, &common.1)
        || !codex_same_identity(&before, &after)
        || !codex_same_version(&before, &after)
        || label.contains(['/', '\\'])
        || label.chars().any(char::is_control)
    {
        return None;
    }
    let branch = if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        if branch.ends_with('.')
            || branch.contains("..")
            || branch.contains("@{")
            || branch
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
            || branch
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
        {
            return None;
        }
        Some(branch.to_owned())
    } else if matches!(head.len(), 40 | 64)
        && head.bytes().all(|b| b.is_ascii_hexdigit())
        && head.bytes().any(|b| b != b'0')
    {
        None
    } else {
        return None;
    };
    Some((label.to_owned(), branch))
}

/// Strict owner-only write check (`0o022 == 0`); zero shared-group tolerance.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn agy_strict_stat(stat: &Stat) -> bool {
    stat.st_uid == rustix::process::geteuid().as_raw() && stat.st_mode & 0o022 == 0
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn agy_strict_ancestor(stat: &Stat, euid: u32) -> bool {
    (stat.st_uid == euid && stat.st_mode & 0o022 == 0)
        || (stat.st_uid == 0 && (stat.st_mode & 0o022 == 0 || (stat.st_mode & 0o1000) != 0))
}

/// Opens an absolute, already-normal path component-by-component without following
/// symlinks, enforcing strict ancestor ownership/modes and `agy_strict_stat` on leaf.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn agy_dir_strict(path: &Path) -> Option<(OwnedFd, Stat)> {
    let rest = path.to_str()?.strip_prefix('/')?;
    if rest
        .split('/')
        .any(|name| name.is_empty() || name == "." || name == "..")
    {
        return None;
    }
    let euid = rustix::process::geteuid().as_raw();
    let mut dir = fs::open("/", CODEX_DIR_FLAGS, Mode::empty()).ok()?;
    let root_stat = fs::fstat(&dir).ok()?;
    if FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory
        || !agy_strict_ancestor(&root_stat, euid)
    {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').collect();
    for (idx, name) in parts.iter().enumerate() {
        dir = fs::openat(&dir, *name, CODEX_DIR_FLAGS, Mode::empty()).ok()?;
        let stat = fs::fstat(&dir).ok()?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return None;
        }
        let last = idx + 1 == parts.len();
        let ok = if last {
            agy_strict_stat(&stat)
        } else {
            agy_strict_ancestor(&stat, euid)
        };
        if !ok {
            return None;
        }
        if last {
            return Some((dir, stat));
        }
    }
    None
}

type AgyWorkspaceTrail = Vec<(PathBuf, AgyWorkspaceEntry)>;
type AgyGitCommon = (PathBuf, OwnedFd, Stat);

enum AgyWorkspaceEntry {
    Directory(Stat, Option<oms_safe_fs::Root>),
    Metadata(Stat),
    Missing,
}

#[derive(Default)]
struct AgyWorkspaceAuthority<'a> {
    group: Option<&'a dyn Fn(u32) -> bool>,
    proven_groups: BTreeMap<u32, bool>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl AgyWorkspaceAuthority<'_> {
    fn private_group(&mut self, gid: u32) -> bool {
        let Some(group) = self.group else {
            return false;
        };
        *self.proven_groups.entry(gid).or_insert_with(|| group(gid))
    }

    fn directory(&mut self, path: &Path, trail: &mut AgyWorkspaceTrail) -> Option<(OwnedFd, Stat)> {
        if let Some((fd, stat)) = agy_dir_strict(path) {
            trail.push((path.to_owned(), AgyWorkspaceEntry::Directory(stat, None)));
            return Some((fd, stat));
        }
        self.group?;
        let root =
            oms_safe_fs::Root::open_private_workspace(path, |gid| self.private_group(gid)).ok()?;
        let fd = root
            .pinned_private_directory()
            .ok()?
            .try_clone_to_owned()
            .ok()?;
        let stat = fs::fstat(&fd).ok()?;
        if stat.st_uid != rustix::process::geteuid().as_raw() {
            return None;
        }
        trail.push((
            path.to_owned(),
            AgyWorkspaceEntry::Directory(stat, Some(root)),
        ));
        Some((fd, stat))
    }

    fn child_directory(
        &mut self,
        dir: &OwnedFd,
        parent: &Path,
        name: &str,
        trail: &mut AgyWorkspaceTrail,
    ) -> Option<(OwnedFd, Stat)> {
        let expected = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        let child = self.directory(&parent.join(name), trail)?;
        let named = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        (workspace_same_directory(&expected, &child.1)
            && workspace_same_directory(&named, &child.1))
        .then_some(child)
    }

    fn allows_file(&mut self, fd: &OwnedFd, stat: &Stat) -> bool {
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_nlink == 1
            && (agy_strict_stat(stat)
                || (self.group.is_some()
                    && oms_safe_fs::workspace_file_has_mode_only_group_access(fd, stat)
                    && self.private_group(stat.st_gid)))
    }

    fn metadata(
        &mut self,
        dir: &OwnedFd,
        parent: &Path,
        name: &str,
        bytes: &mut usize,
        trail: &mut AgyWorkspaceTrail,
    ) -> Option<String> {
        let before = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        let (value, opened) = agy_meta_checked(dir, name, bytes, |fd, stat| {
            workspace_same_file(&before, stat) && self.allows_file(fd, stat)
        })?;
        let named = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        if !workspace_same_file(&named, &opened) {
            return None;
        }
        trail.push((parent.join(name), AgyWorkspaceEntry::Metadata(opened)));
        Some(value)
    }

    fn confirm_entry(&mut self, path: &Path, entry: &AgyWorkspaceEntry) -> Option<()> {
        match entry {
            AgyWorkspaceEntry::Directory(expected, held) => {
                let now = if let Some(root) = held {
                    root.recheck_private_workspace(|gid| self.private_group(gid))
                        .ok()?;
                    fs::fstat(root.pinned_private_directory().ok()?).ok()?
                } else {
                    agy_dir_strict(path)?.1
                };
                workspace_same_directory(expected, &now).then_some(())
            }
            AgyWorkspaceEntry::Metadata(expected) => {
                let (parent, name) = (path.parent()?, path.file_name()?);
                let (dir, _) = self.directory(parent, &mut Vec::new())?;
                let fd = fs::openat(&dir, name, AGY_FILE_FLAGS, Mode::empty()).ok()?;
                let now = fs::fstat(&fd).ok()?;
                let named = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
                (workspace_same_file(expected, &now)
                    && workspace_same_file(&named, &now)
                    && self.allows_file(&fd, &now))
                .then_some(())
            }
            AgyWorkspaceEntry::Missing => {
                let (parent, name) = (path.parent()?, path.file_name()?);
                let (dir, _) = self.directory(parent, &mut Vec::new())?;
                matches!(
                    fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW),
                    Err(rustix::io::Errno::NOENT)
                )
                .then_some(())
            }
        }
    }
}

fn workspace_same_directory(a: &Stat, b: &Stat) -> bool {
    FileType::from_raw_mode(a.st_mode) == FileType::Directory
        && FileType::from_raw_mode(b.st_mode) == FileType::Directory
        && agy_same(a, b)
        && (a.st_uid, a.st_gid, a.st_mode) == (b.st_uid, b.st_gid, b.st_mode)
}

fn workspace_same_file(a: &Stat, b: &Stat) -> bool {
    codex_same_identity(a, b) && codex_same_version(a, b) && a.st_gid == b.st_gid
}

/// Direct workspace and registered linked-checkout authority. Only explicit
/// reads may supply a private-group proof. The proof never applies to AGY logs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct AgyStrictWorkspaces<'a> {
    root: String,
    repo: PathBuf,
    common: Option<Option<AgyGitCommon>>,
    verdicts: BTreeMap<String, bool>,
    bytes: usize,
    trail: AgyWorkspaceTrail,
    authority: AgyWorkspaceAuthority<'a>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl<'a> AgyStrictWorkspaces<'a> {
    pub(crate) fn new(repo: &Path) -> Option<Self> {
        Self::with_group(repo, None)
    }

    pub(crate) fn with_group(repo: &Path, group: Option<&'a dyn Fn(u32) -> bool>) -> Option<Self> {
        if repo.canonicalize().ok().as_deref() != Some(repo) {
            return None;
        }
        let repo_str = repo.to_str()?;
        let mut result = Self {
            root: format!("file://{repo_str}"),
            repo: repo.to_path_buf(),
            common: None,
            verdicts: BTreeMap::new(),
            bytes: AGY_META_BUDGET,
            trail: Vec::new(),
            authority: AgyWorkspaceAuthority {
                group,
                ..Default::default()
            },
        };
        let (repo_fd, _) = result.authority.directory(repo, &mut result.trail)?;
        match fs::statat(&repo_fd, ".git", AtFlags::SYMLINK_NOFOLLOW) {
            Ok(dot) => match FileType::from_raw_mode(dot.st_mode) {
                FileType::Directory => {
                    let common = result.authority.child_directory(
                        &repo_fd,
                        repo,
                        ".git",
                        &mut result.trail,
                    )?;
                    result.common = Some(Some((repo.join(".git"), common.0, common.1)));
                }
                FileType::RegularFile => {
                    let fd = fs::openat(&repo_fd, ".git", AGY_FILE_FLAGS, Mode::empty()).ok()?;
                    let now = fs::fstat(&fd).ok()?;
                    if !workspace_same_file(&dot, &now) || !result.authority.allows_file(&fd, &now)
                    {
                        return None;
                    }
                    result
                        .trail
                        .push((repo.join(".git"), AgyWorkspaceEntry::Metadata(now)));
                    if group.is_some() {
                        // A relaxed direct worktree must prove its own backlink, even
                        // when the child record names only that current checkout.
                        let common = agy_common_strict(
                            repo,
                            &mut result.authority,
                            &mut result.bytes,
                            &mut result.trail,
                        )?;
                        agy_linked_strict(
                            &common,
                            &result.root,
                            &mut result.authority,
                            &mut result.bytes,
                            &mut result.trail,
                        )?;
                        result.common = Some(Some(common));
                    }
                }
                _ => return None,
            },
            Err(rustix::io::Errno::NOENT) => {
                result
                    .trail
                    .push((repo.join(".git"), AgyWorkspaceEntry::Missing));
            }
            Err(_) => return None,
        }
        Some(result)
    }

    pub(crate) fn accepts(&mut self, uri: &str) -> bool {
        if uri == self.root {
            return true;
        }
        if let Some(&verdict) = self.verdicts.get(uri) {
            return verdict;
        }
        if self.verdicts.len() >= AGY_URIS_PER_FRAME {
            return false;
        }
        let (repo, authority, bytes, trail) = (
            &self.repo,
            &mut self.authority,
            &mut self.bytes,
            &mut self.trail,
        );
        let common = self.common.get_or_insert_with(|| {
            let mut local = Vec::new();
            let found = agy_common_strict(repo, authority, bytes, &mut local)?;
            trail.extend(local);
            Some(found)
        });
        let verdict = common.as_ref().is_some_and(|common| {
            let mut local = Vec::new();
            if agy_linked_strict(common, uri, authority, bytes, &mut local).is_some() {
                trail.extend(local);
                true
            } else {
                false
            }
        });
        self.verdicts.insert(uri.to_owned(), verdict);
        verdict
    }

    pub(crate) fn confirm(&self) -> bool {
        self.confirm_group_membership()
            .is_some_and(|proof| proof.recheck())
    }

    pub(crate) fn confirm_group_membership(&self) -> Option<AgyWorkspaceConfirmation<'_>> {
        let mut fresh = AgyWorkspaceAuthority {
            group: self.authority.group,
            ..Default::default()
        };
        let confirmed = self.trail.iter().all(|(_, entry)| match entry {
            AgyWorkspaceEntry::Directory(_, Some(root)) => root
                .recheck_private_workspace(|gid| fresh.private_group(gid))
                .is_ok(),
            AgyWorkspaceEntry::Metadata(stat) if !agy_strict_stat(stat) => {
                fresh.private_group(stat.st_gid)
            }
            _ => true,
        });
        confirmed.then_some(AgyWorkspaceConfirmation {
            trail: &self.trail,
            groups: fresh.proven_groups,
        })
    }
}

pub(crate) struct AgyWorkspaceConfirmation<'a> {
    trail: &'a AgyWorkspaceTrail,
    groups: BTreeMap<u32, bool>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl AgyWorkspaceConfirmation<'_> {
    /// Re-walk names and ACL metadata using the groups checked before the pane
    /// fence. This stage must never call the host account database again.
    pub(crate) fn recheck(&self) -> bool {
        let confirmed = |gid| self.groups.get(&gid) == Some(&true);
        let mut authority = AgyWorkspaceAuthority {
            group: (!self.groups.is_empty()).then_some(&confirmed),
            ..Default::default()
        };
        self.trail
            .iter()
            .all(|(path, entry)| authority.confirm_entry(path, entry).is_some())
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn agy_common_strict(
    repo: &Path,
    authority: &mut AgyWorkspaceAuthority<'_>,
    bytes: &mut usize,
    trail: &mut AgyWorkspaceTrail,
) -> Option<AgyGitCommon> {
    let (repo_fd, _) = authority.directory(repo, trail)?;
    let dot = fs::statat(&repo_fd, ".git", AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(dot.st_mode) == FileType::Directory {
        let (fd, stat) = authority.child_directory(&repo_fd, repo, ".git", trail)?;
        return Some((repo.join(".git"), fd, stat));
    }
    let pointer = authority.metadata(&repo_fd, repo, ".git", bytes, trail)?;
    let admin = agy_join(repo, pointer.strip_prefix("gitdir: ")?)?;
    let (admin_fd, admin_stat) = authority.directory(&admin, trail)?;
    match fs::statat(&admin_fd, "commondir", AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => {
            trail.push((admin.join("commondir"), AgyWorkspaceEntry::Missing));
            Some((admin, admin_fd, admin_stat))
        }
        Err(_) => None,
        Ok(_) => {
            let shared = authority.metadata(&admin_fd, &admin, "commondir", bytes, trail)?;
            let common_path = agy_join(&admin, &shared)?;
            let (common_fd, common_stat) = authority.directory(&common_path, trail)?;
            Some((common_path, common_fd, common_stat))
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn agy_linked_strict(
    common: &AgyGitCommon,
    uri: &str,
    authority: &mut AgyWorkspaceAuthority<'_>,
    bytes: &mut usize,
    trail: &mut AgyWorkspaceTrail,
) -> Option<()> {
    let path = uri.strip_prefix("file://")?;
    if path.contains(['%', '?', '#', '\\']) || path.chars().any(char::is_control) {
        return None;
    }
    let tree = Path::new(path);
    let (tree_fd, tree_stat) = authority.directory(tree, trail)?;
    let pointer = authority.metadata(&tree_fd, tree, ".git", bytes, trail)?;
    let admin = agy_join(tree, pointer.strip_prefix("gitdir: ")?)?;
    let name = admin.file_name()?.to_str()?;
    let worktrees = admin.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let worktrees_parent = worktrees.parent()?;
    let (_, worktrees_parent_stat) = authority.directory(worktrees_parent, trail)?;
    if !workspace_same_directory(&worktrees_parent_stat, &common.2) {
        return None;
    }
    let (registry, _) = authority.child_directory(&common.1, &common.0, "worktrees", trail)?;
    let (record, record_stat) =
        authority.child_directory(&registry, &common.0.join("worktrees"), name, trail)?;
    let backlink = authority.metadata(&record, &admin, "gitdir", bytes, trail)?;
    let shared = authority.metadata(&record, &admin, "commondir", bytes, trail)?;
    let (_, admin_stat) = authority.directory(&admin, trail)?;
    let (backlink, shared) = (agy_join(&admin, &backlink)?, agy_join(&admin, &shared)?);
    if !workspace_same_directory(&record_stat, &admin_stat) || backlink != tree.join(".git") {
        return None;
    }
    let (_, shared_dir_stat) = authority.directory(&shared, trail)?;
    let (_, tree_recheck_stat) = authority.directory(tree, trail)?;
    let common_now = fs::fstat(&common.1).ok()?;
    (workspace_same_directory(&shared_dir_stat, &common.2)
        && workspace_same_directory(&tree_recheck_stat, &tree_stat)
        && workspace_same_directory(&common_now, &common.2))
    .then_some(())
}

fn agy_delivery<'a>(content: &'a str, child_id: &str) -> Option<&'a str> {
    const PREAMBLE: &str = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
    let rest = content.strip_prefix(PREAMBLE).unwrap_or(content);
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    let body = rest
        .strip_prefix("<SYSTEM_MESSAGE>\n")?
        .strip_suffix("\n</SYSTEM_MESSAGE>")?
        .strip_prefix("[Message] ")?;
    body.rsplit("\n[Message] ").find_map(|part| {
        let (hdr, msg_body) = part.split_once(" content=")?;
        if hdr.contains(['\n', '\r']) || msg_body.contains("</SYSTEM_MESSAGE>") {
            return None;
        }
        let mut parts = hdr.split(' ');
        let ts = parts.next()?.strip_prefix("timestamp=")?;
        let sender = parts.next()?.strip_prefix("sender=")?;
        if let Some(extra) = parts.next() {
            let p = extra.strip_prefix("priority=")?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return None;
            }
        }
        let ok = parts.next().is_none()
            && !ts.is_empty()
            && ts.bytes().all(|b| b.is_ascii_graphic())
            && sender == child_id;
        ok.then(|| msg_body.trim())
    })
}

fn agy_title(tool: &Value) -> String {
    let args = &tool["args"];
    args["Role"]
        .as_str()
        .or_else(|| args["role"].as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Native research")
        .trim_matches('"')
        .to_owned()
}

#[cfg(test)]
pub(crate) use tests::seed_codex_chat_rollouts;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use tests::{seed_codex_overview_rollouts, seed_codex_watch_rollouts};

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const CHILD: &str = "11111111-1111-1111-1111-111111111111";
    const SECOND_CHILD: &str = "22222222-2222-2222-2222-222222222222";
    const TURN: &str = "1234567890abcdef1234";
    const STALE_TURN: &str = "abcdef1234567890abcd";
    const WORKSPACE: &str = "file:///fixture/repo";

    #[test]
    fn claude_completion_parses_only_the_exact_top_level_header() {
        let head = "<task-notification>\n<task-id>aa</task-id>\n<tool-use-id>tt</tool-use-id>\n";
        let ok = |body: &str| claude_completion(&format!("{head}{body}"), "aa", "tt");
        assert!(ok(
            "<output-file>/x/y</output-file>\n<status>completed</status>\n<summary>s</summary>\n"
        ));
        // Legacy notices carry no output file.
        assert!(ok("<status>completed</status>\n"));
        assert!(!ok("<status>failed</status>\n"));
        assert!(!ok("<status>completed</status>"));
        assert!(!ok(
            "<output-file>a<b</output-file>\n<status>completed</status>\n"
        ));
        assert!(!ok(
            "<output-file>a</output-file><status>completed</status>\n"
        ));
        assert!(!ok("<summary><status>completed</status>\n</summary>\n"));
        assert!(!ok(
            "<output-file>a</output-file>\n<output-file>b</output-file>\n<status>completed</status>\n"
        ));
        let reordered = "<task-notification>\n<tool-use-id>tt</tool-use-id>\n<task-id>aa</task-id>\n<status>completed</status>\n";
        assert!(!claude_completion(reordered, "aa", "tt"));
        assert!(!claude_completion(
            &format!("x{head}<status>completed</status>\n"),
            "aa",
            "tt"
        ));
        let other = "<task-notification>\n<task-id>bb</task-id>\n<tool-use-id>uu</tool-use-id>\n<status>completed</status>\n<result><task-id>aa</task-id>\n<tool-use-id>tt</tool-use-id>\n<status>completed</status>\n</result>\n";
        assert!(!claude_completion(other, "aa", "tt"));
    }

    struct RolloutFixture {
        root: PathBuf,
        store: Store,
        date: chrono::NaiveDate,
    }

    impl RolloutFixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("oms-codex-rollout-{}-{label}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let store = Store::open(&root).unwrap();
            Self {
                root,
                store,
                date: chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
            }
        }

        fn path(&self, date: chrono::NaiveDate, id: &str) -> PathBuf {
            self.store.repo().join(format!(
                "sessions/{}/rollout-{}T15-01-01-{id}.jsonl",
                date.format("%Y/%m/%d"),
                date.format("%Y-%m-%d")
            ))
        }

        fn write(&self, date: chrono::NaiveDate, id: &str, rows: &[Value]) {
            let path = self.path(date, id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut data = rows
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            data.push('\n');
            std::fs::write(path, data).unwrap();
        }

        fn rollouts(&self) -> CodexRollouts {
            codex_candidates(self.store.repo(), self.date).unwrap()
        }

        fn observe(&self, turn: &str) -> Option<Vec<Value>> {
            Some(self.projection(turn)?.rows)
        }

        fn projection(&self, turn: &str) -> Option<ObservedChildren> {
            let mut remaining = CODEX_READ_BUDGET;
            codex_rows(&self.rollouts(), self.store.repo(), turn, &mut remaining)
                .map(|(observed, _)| observed)
        }

        fn projection_at(&self, turn: &str, now_ms: u64) -> Option<ObservedChildren> {
            let mut remaining = CODEX_READ_BUDGET;
            codex_rows_at(
                &self.rollouts(),
                self.store.repo(),
                turn,
                &mut remaining,
                now_ms,
            )
            .map(|(observed, _)| observed)
        }

        fn chat(&self, terminal: &str, locator: &CodexLocator) -> Option<String> {
            if !codex_chat_turn_matches(terminal, locator) {
                return None;
            }
            codex_chat_from(&self.rollouts(), self.store.repo(), &locator.turn, locator)
        }
    }

    impl Drop for RolloutFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn codex_prompt(turn: &str) -> String {
        format!("{TASK_PREFIX} Turn {turn}. You own this provider's work. Unused task body")
    }

    fn codex_meta(id: &str, repo: &Path, source: Value) -> Value {
        json!({
            "timestamp": "2026-11-18T15:01:01Z", "type": "session_meta",
            "payload": {"id": id, "cwd": repo, "source": source, "thread_source": "user"}
        })
    }

    fn codex_user_event(id: &str, turn: &str) -> Value {
        json!({"type": "event_msg", "payload": {
            "type": "item_completed", "thread_id": id, "item": {
                "type": "UserMessage", "content": [{"type": "text", "text": codex_prompt(turn)}]
            }
        }})
    }

    fn codex_child_meta(id: &str, repo: &Path) -> Value {
        codex_meta(
            id,
            repo,
            json!({"subagent": {"thread_spawn": {"parent_thread_id": PARENT}}}),
        )
    }

    #[cfg(target_os = "linux")]
    fn private_codex_day_fixture(label: &str) -> RolloutFixture {
        use std::os::unix::fs::DirBuilderExt;

        let mut fixture = RolloutFixture::new(label);
        fixture.date = codex_today().unwrap();
        let path = fixture.path(fixture.date, PARENT);
        let day = path.parent().unwrap();
        std::fs::create_dir_all(day.parent().unwrap()).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(day).unwrap();
        fixture
    }

    #[cfg(target_os = "linux")]
    fn sample_codex_day_wake(
        fixture: &RolloutFixture,
        prior: Option<&CodexWake>,
        stop: &AtomicBool,
    ) -> Option<(ObservedChildren, CodexWake)> {
        let (home, root) = open_ambient_codex_root(fixture.store.repo())?;
        let terminal = format!("› {}", codex_prompt(TURN));
        let mut observed = observe_codex(
            fixture.store.repo(),
            &terminal,
            &home,
            &root,
            stop,
            Some(CodexWakeRequest { previous: prior }),
        )?;
        let wake = observed.codex_wake.take()?;
        Some((observed, wake))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_day_wake_tracks_child_only_writes_across_partial_frames() {
        use crate::wakeup::ChildWakeSource;
        use std::io::Write;

        let fixture = private_codex_day_fixture("day-child-only");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("cli")),
                codex_user_event(PARENT, STALE_TURN),
                codex_user_event(PARENT, TURN),
            ],
        );
        fixture.write(
            fixture.date,
            SECOND_CHILD,
            &[codex_child_meta(SECOND_CHILD, repo)],
        );
        let parent = fixture.path(fixture.date, PARENT);
        let parent_bytes = std::fs::read(&parent).unwrap();
        let stop = AtomicBool::new(false);
        let (history, wake) = sample_codex_day_wake(&fixture, None, &stop).unwrap();
        assert_eq!(history.rows.len(), 1);
        assert_eq!(
            history.rows[0]["participant"],
            participant_id("codex", SECOND_CHILD)
        );

        // The process creates the child after scanning, before the Board can
        // accept the frame. The parent stays quiet through both child writes.
        let mut child = File::create(fixture.path(fixture.date, CHILD)).unwrap();
        let header = codex_child_meta(CHILD, repo).to_string();
        let (partial, rest) = header.split_at(8);
        child.write_all(partial.as_bytes()).unwrap();
        assert_eq!(wake.source.changed(), Some(true));
        let (still_history, same_wake) =
            sample_codex_day_wake(&fixture, Some(&wake), &stop).unwrap();
        assert_eq!(still_history.rows, history.rows);
        assert!(Arc::ptr_eq(&wake.source, &same_wake.source));

        writeln!(child, "{rest}").unwrap();
        assert_eq!(same_wake.source.changed(), Some(true));
        let (ready, retained) = sample_codex_day_wake(&fixture, Some(&same_wake), &stop).unwrap();
        assert_eq!(ready.rows.len(), 2);
        assert!(
            ready
                .rows
                .iter()
                .any(|row| row["participant"] == participant_id("codex", CHILD))
        );
        assert_eq!(std::fs::read(&parent).unwrap(), parent_bytes);

        writeln!(child).unwrap();
        drop(child);
        assert_eq!(retained.source.changed(), Some(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_day_wake_recovers_prearm_partial_and_coalesces_only_live_day() {
        use crate::wakeup::{ChildWakeSource, FirstChildWake};
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let fixture = private_codex_day_fixture("day-scope");
        let foreign = RolloutFixture::new("day-foreign");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("cli")),
                codex_user_event(PARENT, TURN),
            ],
        );
        let path = fixture.path(fixture.date, CHILD);
        let day = path.parent().unwrap();
        let mut child = File::create(&path).unwrap();
        let header = codex_child_meta(CHILD, repo).to_string();
        let (partial, rest) = header.split_at(8);
        child.write_all(partial.as_bytes()).unwrap();

        let stop = AtomicBool::new(false);
        let (empty, observed) = sample_codex_day_wake(&fixture, None, &stop).unwrap();
        let source = &observed.source;
        assert!(empty.rows.is_empty());
        foreign.write(foreign.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        assert_eq!(source.changed(), Some(false));
        fixture.write(
            fixture.date,
            SECOND_CHILD,
            &[codex_child_meta(SECOND_CHILD, foreign.store.repo())],
        );
        assert_eq!(source.changed(), Some(true));
        let (foreign_frame, _) = sample_codex_day_wake(&fixture, Some(&observed), &stop).unwrap();
        assert!(foreign_frame.rows.is_empty());
        std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(source.changed(), Some(false));

        let now = Instant::now();
        let mut wake = FirstChildWake::default();
        wake.replace(Some(source.clone()));
        for byte in rest.as_bytes().chunks(10) {
            child.write_all(byte).unwrap();
        }
        writeln!(child).unwrap();
        assert!(!wake.refresh(true, false, now));
        assert!(wake.fd().is_none());
        assert!(wake.refresh(false, true, now));
        assert!(!wake.refresh(false, true, now + Duration::from_millis(100)));
        let (ready, _) = sample_codex_day_wake(&fixture, Some(&observed), &stop).unwrap();
        assert_eq!(ready.rows.len(), 1);

        std::fs::set_permissions(day, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(!source.current());
        assert_eq!(source.changed(), None);
        assert!(sample_codex_day_wake(&fixture, None, &stop).is_none());
        std::fs::set_permissions(day, std::fs::Permissions::from_mode(0o700)).unwrap();

        let (_, reopened) = sample_codex_day_wake(&fixture, None, &stop).unwrap();
        let retired = day.with_extension("old");
        std::fs::rename(day, &retired).unwrap();
        std::fs::create_dir(day).unwrap();
        assert!(!reopened.source.current());
        assert_eq!(reopened.source.changed(), None);
        assert!(sample_codex_day_wake(&fixture, Some(&reopened), &stop).is_none());

        stop.store(true, Ordering::Release);
        assert!(sample_codex_day_wake(&fixture, None, &stop).is_none());
        wake.replace(None);
        assert!(wake.fd().is_none());
        assert!(!wake.refresh(true, true, now + Duration::from_secs(2)));
    }

    pub(crate) fn seed_codex_chat_rollouts(
        home: &Path,
        repo: &Path,
        turn: &str,
        title: &str,
        message: &str,
    ) -> String {
        let today = codex_today().unwrap();
        let dir = home
            .join("sessions")
            .join(today.format("%Y/%m/%d").to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let write = |id, rows: &[Value]| {
            let mut data = rows
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            data.push('\n');
            std::fs::write(
                dir.join(format!(
                    "rollout-{}T15-01-01-{id}.jsonl",
                    today.format("%Y-%m-%d")
                )),
                data,
            )
            .unwrap();
        };
        let mut child = codex_child_meta(CHILD, repo);
        child["payload"]["source"]["subagent"]["thread_spawn"]["agent_nickname"] = json!(title);
        write(
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_user_event(PARENT, turn),
            ],
        );
        write(
            CHILD,
            &[
                child,
                codex_context("reply-turn", repo),
                codex_agent_event(CHILD, "reply-turn", message),
            ],
        );
        participant_id("codex", CHILD)
    }

    #[cfg(target_os = "linux")]
    fn codex_watch_rollout_dir(home: &Path) -> (chrono::NaiveDate, PathBuf) {
        let date = codex_today().unwrap();
        let dir = home
            .join("sessions")
            .join(date.format("%Y/%m/%d").to_string());
        std::fs::create_dir_all(&dir).unwrap();
        (date, dir)
    }

    #[cfg(target_os = "linux")]
    fn codex_watch_rollout_path(date: chrono::NaiveDate, dir: &Path, id: &str) -> PathBuf {
        dir.join(format!(
            "rollout-{}T15-01-01-{id}.jsonl",
            date.format("%Y-%m-%d")
        ))
    }

    #[cfg(target_os = "linux")]
    fn codex_watch_child(repo: &Path, id: &str, title: &str) -> Value {
        let mut child = codex_child_meta(id, repo);
        child["payload"]["source"]["subagent"]["thread_spawn"]["agent_nickname"] = json!(title);
        child
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn seed_codex_watch_rollouts(
        home: &Path,
        repo: &Path,
        turn: &str,
        history: &str,
        next: &str,
    ) -> (PathBuf, String) {
        let (date, dir) = codex_watch_rollout_dir(home);
        let path = |id| codex_watch_rollout_path(date, &dir, id);
        let child = |id, title| format!("{}\n", codex_watch_child(repo, id, title));
        std::fs::write(
            path(PARENT),
            format!(
                "{}\n{}\n{}\n",
                codex_meta(PARENT, repo, json!("cli")),
                codex_user_event(PARENT, STALE_TURN),
                codex_user_event(PARENT, turn),
            ),
        )
        .unwrap();
        std::fs::write(path(SECOND_CHILD), child(SECOND_CHILD, history)).unwrap();
        (path(CHILD), child(CHILD, next))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn seed_codex_overview_rollouts(
        home: &Path,
        repo: &Path,
        turn: &str,
        history: [&str; 3],
        next: &str,
    ) -> (PathBuf, String) {
        use std::io::Write as _;

        let (date, dir) = codex_watch_rollout_dir(home);
        let path = |id| codex_watch_rollout_path(date, &dir, id);
        let now = codex_now_ms();
        let typed = |turn, id, at| {
            let mut row = codex_user_event(PARENT, turn);
            row["payload"]["turn_id"] = json!(id);
            row["payload"]["completed_at_ms"] = json!(at);
            row
        };
        std::fs::write(
            path(PARENT),
            format!(
                "{}\n{}\n{}\n",
                codex_meta(PARENT, repo, json!("cli")),
                typed(STALE_TURN, "fixture-old-turn", now - 300_000),
                typed(turn, TURN_ID, now - 2_000),
            ),
        )
        .unwrap();
        let child = |id, title, at| {
            let mut row = codex_watch_child(repo, id, title);
            row["timestamp"] = json!(codex_fixture_time(now));
            row["payload"]["timestamp"] = json!(codex_fixture_time(at));
            format!("{row}\n")
        };
        // Put all three old children first by mtime. Their replayed envelope
        // times also look fresh, while their creation clocks remain historical.
        let historical = [SECOND_CHILD, SIBLINGS[0], SIBLINGS[1]];
        let future_mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        for (id, title) in historical.into_iter().zip(history) {
            let mut file = File::create(path(id)).unwrap();
            file.write_all(child(id, title, now - 240_000).as_bytes())
                .unwrap();
            file.set_modified(future_mtime).unwrap();
        }
        // Check all old headers with the real collector before the timed PTY.
        let rollouts = codex_candidates(home, date).unwrap();
        let mut remaining = CODEX_READ_BUDGET;
        let (accepted, _) = codex_rows_at(&rollouts, repo, turn, &mut remaining, now).unwrap();
        let expected: BTreeSet<_> = historical
            .into_iter()
            .map(|id| participant_id("codex", id))
            .collect();
        assert_eq!(
            accepted.codex.keys().cloned().collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(accepted.rows.len(), history.len());
        assert!(
            accepted
                .rows
                .iter()
                .all(|row| row.get("codex_recent_creation").is_none())
        );
        (path(CHILD), child(CHILD, next, now))
    }

    fn codex_context(turn: &str, cwd: &Path) -> Value {
        json!({"type":"turn_context", "payload":{"turn_id":turn, "cwd":cwd}})
    }

    fn codex_agent_event(id: &str, turn: &str, message: &str) -> Value {
        json!({"type":"event_msg", "payload":{
            "type":"item_completed", "thread_id":id, "turn_id":turn,
            "item":{"type":"AgentMessage", "content":[{"type":"Text", "text":message}]}
        }})
    }

    #[test]
    fn codex_optional_usage_preserves_parent_append_proof_under_tight_budget() {
        let fixture = RolloutFixture::new("usage-budget-append");
        let repo = fixture.store.repo();
        let mut parent = codex_meta(PARENT, repo, json!("vscode"));
        parent["payload"]["session_id"] = json!(PARENT);
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let mut child = codex_child_meta(CHILD, repo);
        child["payload"]["session_id"] = json!(PARENT);
        fixture.write(
            fixture.date,
            CHILD,
            &[child, json!({"padding": "x".repeat(4096)})],
        );
        let rollouts = fixture.rollouts();
        let mut baseline = CODEX_READ_BUDGET;
        let (_, sample) = codex_rows_checked(
            &rollouts,
            repo,
            TURN,
            &mut baseline,
            &AtomicBool::new(false),
            0,
            false,
        )
        .unwrap();
        let proof = sample.header.len() + sample.body.len();
        let mut remaining = CODEX_READ_BUDGET - baseline + proof;
        CODEX_TEST_APPEND_PARENT
            .with(|path| *path.borrow_mut() = Some((fixture.path(fixture.date, PARENT), false)));
        let (observed, _) = codex_rows_checked(
            &rollouts,
            repo,
            TURN,
            &mut remaining,
            &AtomicBool::new(false),
            0,
            true,
        )
        .expect("parent append proof must retain roster");
        assert_eq!(observed.rows.len(), 1);
        assert!(observed.rows[0].get("native_response_usage").is_none());
        assert_eq!(remaining, proof);

        fixture.write(
            fixture.date,
            PARENT,
            &[parent, codex_user_event(PARENT, TURN)],
        );
        let mut remaining = CODEX_READ_BUDGET - baseline;
        CODEX_TEST_APPEND_PARENT
            .with(|path| *path.borrow_mut() = Some((fixture.path(fixture.date, PARENT), false)));
        let (observed, _) = codex_rows_checked(
            &fixture.rollouts(),
            repo,
            TURN,
            &mut remaining,
            &AtomicBool::new(false),
            0,
            true,
        )
        .expect("exhausted optional reserve must retain roster");
        assert_eq!(observed.rows.len(), 1);
        assert!(observed.rows[0].get("native_response_usage").is_none());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn codex_chat_returns_only_text_with_exact_child_turn_context() {
        use std::io::Write;

        let fixture = RolloutFixture::new("chat-ownership");
        let repo = fixture.store.repo();
        let foreign = repo.join("other");
        std::fs::create_dir(&foreign).unwrap();
        let parent = codex_meta(PARENT, repo, json!("vscode"));
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let mut mixed = codex_agent_event(CHILD, "local", "First owned reply");
        mixed["payload"]["item"]["content"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"type":"text", "text":"lowercase block hidden"}),
                json!({"type":"Tool", "text":"tool block hidden"}),
            ]);
        let mut user = codex_agent_event(CHILD, "local", "user input hidden");
        user["payload"]["item"]["type"] = json!("UserMessage");
        let mut bound_to_sibling = codex_context("sibling-context", repo);
        bound_to_sibling["payload"]["thread_id"] = json!(SECOND_CHILD);
        fixture.write(
            fixture.date,
            CHILD,
            &[
                codex_child_meta(CHILD, repo),
                parent,
                codex_agent_event(CHILD, "local", "reply before context hidden"),
                codex_context("local", repo),
                codex_agent_event(PARENT, "local", "inherited reply hidden"),
                codex_agent_event(SECOND_CHILD, "local", "sibling reply hidden"),
                codex_agent_event(CHILD, "another-turn", "wrong-turn reply hidden"),
                mixed,
                user,
                codex_context("foreign", &foreign),
                codex_agent_event(CHILD, "foreign", "foreign reply hidden"),
                bound_to_sibling,
                codex_agent_event(CHILD, "sibling-context", "foreign context thread hidden"),
                codex_context("ambiguous", repo),
                codex_agent_event(CHILD, "ambiguous", "ambiguously attributed reply hidden"),
                codex_context("ambiguous", &foreign),
                codex_context("next-local", repo),
                codex_agent_event(CHILD, "next-local", "\x1b[31mSecond owned reply\x1b[0m"),
            ],
        );

        let observed = fixture.projection(TURN).unwrap();
        assert_eq!(observed.rows.len(), 1);
        let public = &observed.rows[0];
        let projected = public.to_string();
        assert!(!projected.contains(CHILD) && !projected.contains("owned reply"));
        let locator = &observed.codex[text(public, "participant")];
        let terminal = format!("› {}", codex_prompt(TURN));
        assert_eq!(
            fixture.chat(&terminal, locator).as_deref(),
            Some("First owned reply\n\nSecond owned reply")
        );
        let other_turn = format!("{terminal}\n› {}", codex_prompt(STALE_TURN));
        assert!(fixture.chat(&other_turn, locator).is_none());
        assert!(codex_chat_from(&fixture.rollouts(), &foreign, TURN, locator).is_none());

        let sampled = fixture.rollouts();
        let mut appender = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        writeln!(appender, "{}", codex_context("parent-still-running", repo)).unwrap();
        assert_eq!(
            codex_chat_from(&sampled, repo, TURN, locator).as_deref(),
            Some("First owned reply\n\nSecond owned reply"),
        );
        assert!(fixture.chat(&terminal, locator).is_some());

        fixture.write(
            fixture.date.succ_opt().unwrap(),
            CHILD,
            &[
                codex_child_meta(CHILD, repo),
                codex_context("copy", repo),
                codex_agent_event(CHILD, "copy", "ambiguous child hidden"),
            ],
        );
        let ambiguous = fixture.projection(TURN).unwrap();
        assert_eq!(ambiguous.rows.len(), 1);
        assert!(ambiguous.codex.is_empty());
        assert!(fixture.chat(&terminal, locator).is_none());
    }

    #[test]
    fn observed_model_validator_returns_the_input_unchanged_or_none() {
        let leaked = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        for ok in ["gpt-child".to_owned(), "m".repeat(80)] {
            assert_eq!(observed_model(&ok), Some(ok.as_str()));
        }
        for bad in [
            "m".repeat(81),
            String::new(),
            " ".into(),
            " gpt".into(),
            "gpt\n".into(),
            "\u{1b}[31mgpt".into(),
            "gpt\u{0}".into(),
            "\u{9b}31m".into(),
            "<synthetic>".into(),
            "<placeholder>".into(),
            format!("gpt-{leaked}"),
        ] {
            assert_eq!(observed_model(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn codex_child_rows_publish_only_the_proven_turn_context_model() {
        let fixture = RolloutFixture::new("observed-model");
        let repo = fixture.store.repo();
        let mut parent = codex_meta(PARENT, repo, json!("vscode"));
        parent["payload"]["session_id"] = json!(PARENT);
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let mut child = codex_child_meta(CHILD, repo);
        child["payload"]["session_id"] = json!(PARENT);
        let counters = |total| {
            json!({"input_tokens":1, "output_tokens":1, "cached_input_tokens":0,
                "cache_write_input_tokens":0, "reasoning_output_tokens":0, "total_tokens":total})
        };
        let usage = json!({"type":"token_usage_record","payload":{
            "thread_id":CHILD,"session_id":PARENT,"turn_id":"child-turn",
            "root_turn_id":"root-turn","response_id":"r1","usage":counters(2),
            "turn_token_usage":counters(2),"thread_token_usage":counters(2)}});
        let context = |turn: &str, model: Option<&str>| {
            let mut value = codex_context(turn, repo);
            if let Some(model) = model {
                value["payload"]["model"] = json!(model);
            }
            value
        };
        let observed = |contexts: Vec<Value>| {
            let mut rows = vec![child.clone()];
            rows.extend(contexts);
            rows.push(usage.clone());
            fixture.write(fixture.date, CHILD, &rows);
            fixture.projection(TURN).unwrap().rows.remove(0)
        };
        let exact = observed(vec![context("child-turn", Some("gpt-child"))]);
        assert_eq!(exact["observed_model"], "gpt-child");
        assert_eq!(exact["model"], "");
        // Another turn's model and conflicting same-turn contexts prove nothing.
        for contexts in [
            vec![context("other-turn", Some("gpt-other"))],
            vec![
                context("child-turn", Some("gpt-child")),
                context("child-turn", Some("gpt-else")),
            ],
            vec![
                context("child-turn", Some("gpt-child")),
                context("child-turn", None),
            ],
            vec![context("child-turn", Some(" "))],
            vec![],
        ] {
            let row = observed(contexts);
            assert!(row.get("native_response_usage").is_some());
            assert!(row.get("observed_model").is_none(), "{row}");
        }
        // A model the shared validator rejects is omitted; the usage is unchanged.
        let leaked = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        for bad in [
            "m".repeat(81),
            "\u{1b}[31mgpt".to_owned(),
            format!("gpt-{leaked}"),
        ] {
            let row = observed(vec![context("child-turn", Some(&bad))]);
            assert!(row.get("observed_model").is_none(), "{bad:?}: {row}");
            assert_eq!(row["native_response_usage"], exact["native_response_usage"]);
        }
        // A context from a foreign repository is not this child's.
        let mut foreign = context("child-turn", Some("gpt-foreign"));
        foreign["payload"]["cwd"] = json!("/somewhere/else");
        assert!(observed(vec![foreign]).get("observed_model").is_none());
    }

    #[test]
    fn codex_chat_adds_only_the_latest_unambiguous_recorded_response_usage() {
        use std::io::Write;

        let fixture = RolloutFixture::new("chat-response-usage");
        let repo = fixture.store.repo();
        let mut parent = codex_meta(PARENT, repo, json!("vscode"));
        parent["payload"]["session_id"] = json!(PARENT);
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let mut child = codex_child_meta(CHILD, repo);
        child["payload"]["session_id"] = json!(PARENT);
        let counters = |input, output, read, write, reasoning, total| {
            json!({"input_tokens":input, "output_tokens":output,
                "cached_input_tokens":read, "cache_write_input_tokens":write,
                "reasoning_output_tokens":reasoning, "total_tokens":total})
        };
        let record = |id: &str, usage: Value| {
            json!({"type":"token_usage_record","payload":{
                "thread_id":CHILD,"session_id":PARENT,"turn_id":"child-turn",
                "root_turn_id":"root-turn","response_id":id,"usage":usage,
                "turn_token_usage":counters(200,80,20,10,5,280),
                "thread_token_usage":counters(400,120,30,20,10,520)
            }})
        };
        let first = record("response-one", counters(11, 1, 0, 2, 0, 12));
        let latest = record("response-two", counters(14, 5, 7, 3, 2, 19));
        let mut parent_usage = latest.clone();
        parent_usage["payload"]["thread_id"] = json!(PARENT);
        let mut sibling = latest.clone();
        sibling["payload"]["thread_id"] = json!(SECOND_CHILD);
        let mut wrong_session = latest.clone();
        wrong_session["payload"]["session_id"] = json!(SECOND_CHILD);
        let nested = json!({"type":"compacted","payload":{
            "latest_token_usage_record":latest["payload"]
        }});
        let legacy = json!({"type":"event_msg","payload":{
            "type":"token_count","info":{"total_token_usage":counters(400,120,30,20,10,520)}
        }});
        let ready = |header: &Value, usage_rows: &[Value]| {
            let mut rows = vec![
                header.clone(),
                parent.clone(),
                codex_context("child-turn", repo),
                codex_agent_event(CHILD, "child-turn", "Ordinary owned reply"),
            ];
            rows.extend_from_slice(usage_rows);
            fixture.write(fixture.date, CHILD, &rows);
            let observed = fixture.projection(TURN).unwrap();
            assert_eq!(observed.rows.len(), 1);
            if usage_rows == [latest.clone()] && header == &child {
                assert_eq!(
                    observed.rows[0]["native_response_usage"]["response"]["total_tokens"],
                    19
                );
            }
            assert!(
                !observed.rows[0]
                    .to_string()
                    .contains("recorded response usage")
            );
            observed.codex[text(&observed.rows[0], "participant")].clone()
        };
        let terminal = format!("› {}", codex_prompt(TURN));
        let output = |header: &Value, records: &[Value]| {
            fixture.chat(&terminal, &ready(header, records)).unwrap()
        };
        let usage = "Ordinary owned reply\n\nCodex recorded response usage · in 14 · out 5 · cache 7/3 · reasoning 2 · total 19";
        assert_eq!(
            output(
                &child,
                &[
                    first.clone(),
                    latest.clone(),
                    first.clone(),
                    parent_usage.clone(),
                    sibling.clone(),
                    wrong_session.clone(),
                    nested.clone(),
                    legacy.clone(),
                ]
            ),
            usage
        );
        assert_eq!(
            output(
                &child,
                &[parent_usage, sibling, wrong_session, nested, legacy]
            ),
            "Ordinary owned reply"
        );

        let mut conflict = latest.clone();
        conflict["payload"]["usage"] = counters(14, 6, 7, 3, 2, 20);
        assert_eq!(
            output(&child, &[latest.clone(), conflict]),
            "Ordinary owned reply"
        );
        for invalid in [
            json!(-1),
            json!(14.0),
            json!("14"),
            json!(u64::MAX),
            Value::Null,
        ] {
            let mut invalid_record = latest.clone();
            invalid_record["payload"]["usage"]["input_tokens"] = invalid;
            assert_eq!(
                output(&child, std::slice::from_ref(&invalid_record)),
                "Ordinary owned reply"
            );
            assert_eq!(
                output(&child, &[latest.clone(), invalid_record]),
                "Ordinary owned reply"
            );
        }
        let mut empty_turn_usage = latest.clone();
        empty_turn_usage["payload"]["turn_token_usage"] = json!({});
        let mut negative_thread_usage = latest.clone();
        negative_thread_usage["payload"]["thread_token_usage"]["total_tokens"] = json!(-1);
        for invalid_aggregate in [empty_turn_usage, negative_thread_usage] {
            assert_eq!(output(&child, &[invalid_aggregate]), "Ordinary owned reply");
        }

        let mut invalid_id = latest.clone();
        invalid_id["payload"]["response_id"] = json!("two words");
        assert_eq!(output(&child, &[invalid_id]), "Ordinary owned reply");

        let zero = record("response-zero", counters(0, 0, 0, 0, 0, 0));
        assert_eq!(
            output(&child, &[zero]),
            "Ordinary owned reply\n\nCodex recorded response usage · in 0 · out 0 · cache ?/? · reasoning ? · total 0"
        );
        let mut legacy_header = child.clone();
        legacy_header["payload"]
            .as_object_mut()
            .unwrap()
            .remove("session_id");
        assert_eq!(
            output(&legacy_header, std::slice::from_ref(&latest)),
            "Ordinary owned reply"
        );
        let mut mismatched_header = child.clone();
        mismatched_header["payload"]["session_id"] = json!(SECOND_CHILD);
        let mut mismatched_record = latest.clone();
        mismatched_record["payload"]["session_id"] = json!(SECOND_CHILD);
        assert_eq!(
            output(&mismatched_header, &[mismatched_record]),
            "Ordinary owned reply"
        );

        let projected = |header: &Value, records: &[Value]| {
            let mut rows = vec![header.clone()];
            rows.extend_from_slice(records);
            fixture.write(fixture.date, CHILD, &rows);
            fixture.projection(TURN).unwrap().rows.remove(0)
        };
        let mut invalid_latest = latest.clone();
        invalid_latest["payload"]["usage"]["input_tokens"] = json!(-1);
        let mut conflicting_latest = latest.clone();
        conflicting_latest["payload"]["usage"]["total_tokens"] = json!(20);
        let mut invalid_id = latest.clone();
        invalid_id["payload"]["response_id"] = json!("two words");
        let recovered = record("response-three", counters(20, 4, 7, 3, 2, 24));
        // A later owned response withholds stale counters even when an older
        // response is still valid; only a new response can restore them.
        for mut records in [
            vec![first.clone(), invalid_latest],
            vec![first.clone(), latest.clone(), conflicting_latest.clone()],
            vec![
                first.clone(),
                latest.clone(),
                conflicting_latest,
                first.clone(),
            ],
            vec![first.clone(), invalid_id],
        ] {
            assert_eq!(output(&child, &records), "Ordinary owned reply");
            assert!(
                projected(&child, &records)
                    .get("native_response_usage")
                    .is_none()
            );
            records.push(recovered.clone());
            assert_eq!(
                output(&child, &records),
                "Ordinary owned reply\n\nCodex recorded response usage · in 20 · out 4 · cache 7/3 · reasoning 2 · total 24"
            );
            assert_eq!(
                projected(&child, &records)["native_response_usage"]["response"]["total_tokens"],
                24
            );
        }
        let valid = projected(&child, &[latest.clone(), latest.clone()]);
        assert_eq!(valid["native_response_usage"]["turn"]["total_tokens"], 280);
        assert_eq!(
            valid["native_response_usage"]["thread"]["total_tokens"],
            520
        );
        assert_eq!(valid["started_at"], "2026-11-18T15:01:01+00:00");
        assert_eq!(valid["state"], "unknown");
        let mut conflict = latest.clone();
        conflict["payload"]["usage"]["total_tokens"] = json!(20);
        let mut wrong_session = latest.clone();
        wrong_session["payload"]["session_id"] = json!(SECOND_CHILD);
        for records in [vec![], vec![latest.clone(), conflict], vec![wrong_session]] {
            assert!(
                projected(&child, &records)
                    .get("native_response_usage")
                    .is_none()
            );
        }
        let mut invalid_stats = latest.clone();
        invalid_stats["payload"]["thread_token_usage"]["total_tokens"] = json!(-1);
        assert!(
            projected(&child, &[invalid_stats])
                .get("native_response_usage")
                .is_none()
        );
        let mut mismatched = child.clone();
        mismatched["payload"]["session_id"] = json!(SECOND_CHILD);
        assert!(
            projected(&mismatched, std::slice::from_ref(&latest))
                .get("native_response_usage")
                .is_none()
        );
        let mut bad_clock = child.clone();
        bad_clock["timestamp"] = json!("invalid");
        assert!(projected(&bad_clock, &[]).get("started_at").is_none());
        fixture.write(fixture.date, CHILD, &[child.clone(), latest.clone()]);
        let rollouts = fixture.rollouts();
        let rollout = rollouts
            .candidates
            .iter()
            .find(|r| codex_filename_id(&r.path) == Some(CHILD))
            .unwrap();
        let mut budget = 0;
        assert!(
            codex_child_usage(
                &rollouts,
                rollout,
                repo,
                CHILD,
                PARENT,
                parent.to_string().as_bytes(),
                &mut budget
            )
            .is_none()
        );
        assert_eq!(budget, 0);

        let locator = ready(&child, std::slice::from_ref(&latest));
        let child_path = fixture.path(fixture.date, CHILD);
        let duplicate_key = latest.to_string().replace(
            r#""input_tokens":14"#,
            r#""input_tokens":14,"input_tokens":15"#,
        );
        assert!(duplicate_key.contains(r#""input_tokens":15"#));
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&child_path)
                .unwrap(),
            "{duplicate_key}"
        )
        .unwrap();
        assert_eq!(
            fixture.chat(&terminal, &locator).as_deref(),
            Some("Ordinary owned reply")
        );
        assert!(
            fixture.projection(TURN).unwrap().rows[0]
                .get("native_response_usage")
                .is_none()
        );
        let locator = ready(&child, std::slice::from_ref(&latest));
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&child_path)
                .unwrap(),
            r#"{{"type":"event_msg","payload":{{"type":"item_completed","type":"other"}}}}"#
        )
        .unwrap();
        assert_eq!(
            fixture.chat(&terminal, &locator).as_deref(),
            Some("Ordinary owned reply")
        );
        assert!(
            fixture.projection(TURN).unwrap().rows[0]
                .get("native_response_usage")
                .is_none()
        );
        ready(&child, std::slice::from_ref(&latest));
        CODEX_TEST_APPEND_PARENT
            .with(|path| *path.borrow_mut() = Some((fixture.path(fixture.date, PARENT), true)));
        let observed = fixture
            .projection(TURN)
            .expect("failed optional proof retains roster");
        assert_eq!(observed.rows.len(), 1);
        assert_eq!(observed.codex.len(), 1);
        assert!(observed.rows[0].get("native_response_usage").is_none());
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let locator = ready(&child, std::slice::from_ref(&latest));
        assert_eq!(fixture.chat(&terminal, &locator).as_deref(), Some(usage));
        let prior = child_path.with_extension("prior");
        std::fs::rename(&child_path, &prior).unwrap();
        std::fs::copy(&prior, &child_path).unwrap();
        assert!(fixture.chat(&terminal, &locator).is_none());
    }

    #[test]
    fn codex_chat_bounds_child_suffix_and_redacts_before_display_clipping() {
        use std::io::Write;

        let fixture = RolloutFixture::new("chat-bounds");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_user_event(PARENT, TURN),
            ],
        );
        fixture.write(
            fixture.date,
            CHILD,
            &[
                codex_child_meta(CHILD, repo),
                codex_context("stale", repo),
                codex_agent_event(CHILD, "stale", "stale-prefix-hidden"),
            ],
        );
        let projected = fixture.projection(TURN).unwrap();
        let locator = &projected.codex[text(&projected.rows[0], "participant")];
        let path = fixture.path(fixture.date, CHILD);
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(2 * CODEX_CHAT_WINDOW as u64))
            .unwrap();
        // Both the read window and the output cap must apply independently.
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        let colored = format!("ghp\x1b[31m{}", &sample[3..]);
        let bounded = format!(
            "{sample}\n{}\n{colored}\n안녕 latest",
            "x".repeat(CODEX_CHAT_LIMIT - 65)
        );
        let mut oversized = codex_agent_event(CHILD, "tail", "overlong-message-hidden");
        oversized["padding"] = json!("x".repeat(MAX_LINE));
        file.write_all(b"\n").unwrap();
        for value in [
            codex_context("tail", repo),
            oversized,
            codex_agent_event(CHILD, "tail", "message with lost context hidden"),
            codex_context("fresh", repo),
            codex_agent_event(CHILD, "fresh", &bounded),
        ] {
            writeln!(file, "{value}").unwrap();
        }
        write!(
            file,
            "{}",
            codex_agent_event(CHILD, "fresh", "unterminated hidden")
        )
        .unwrap();
        let terminal = format!("› {}", codex_prompt(TURN));
        let history = fixture.chat(&terminal, locator).unwrap();
        assert_eq!(history.len(), CODEX_CHAT_LIMIT);
        assert!(history.ends_with("[oms-redacted:github-token]\n안녕 latest"));
        assert!(!history.contains(&sample[sample.len() - 8..]) && !history.contains("\x1b"));
        assert!(!history.contains("hidden"));

        for id in [SECOND_CHILD, "33333333-3333-3333-3333-333333333333"] {
            fixture.write(fixture.date, id, &[codex_meta(id, repo, json!("vscode"))]);
            std::fs::OpenOptions::new()
                .write(true)
                .open(fixture.path(fixture.date, id))
                .unwrap()
                .set_len(15 * CODEX_READ_BUDGET as u64 / 32)
                .unwrap();
        }
        // Projection fits, but the target's additional one MiB must share its budget.
        assert!(
            fixture
                .projection_at(TURN, locator.observed_ms)
                .unwrap()
                .codex
                .values()
                .any(|entry| entry == locator)
        );
        assert!(fixture.chat(&terminal, locator).is_none());
    }

    #[test]
    fn codex_shows_only_first_header_children_of_bound_parent() {
        use std::io::Write;

        let fixture = RolloutFixture::new("exact-parent");
        let repo = fixture.store.repo();
        let parent = codex_meta(PARENT, repo, json!("vscode"));
        fixture.write(
            fixture.date,
            PARENT,
            &[parent.clone(), codex_user_event(PARENT, TURN)],
        );
        let spawn = |id| json!({"subagent":{"thread_spawn":{"parent_thread_id": id}}});
        let first_header = codex_meta(CHILD, repo, spawn(PARENT));
        fixture.write(fixture.date, CHILD, &[first_header, parent.clone(), json!({
            "type":"event_msg", "payload":{"type":"task_complete", "last_agent_message":"unsettled"}
        })]);
        let foreign_repo = repo.join("other");
        std::fs::create_dir(&foreign_repo).unwrap();
        fixture.write(
            fixture.date,
            SECOND_CHILD,
            &[codex_meta(SECOND_CHILD, &foreign_repo, spawn(PARENT))],
        );
        let other = "33333333-3333-3333-3333-333333333333";
        fixture.write(
            fixture.date,
            other,
            &[codex_meta(other, repo, spawn(CHILD))],
        );
        let foreign_header = "44444444-4444-4444-4444-444444444444";
        fixture.write(
            fixture.date,
            foreign_header,
            &[parent, codex_meta(foreign_header, repo, spawn(PARENT))],
        );
        let empty = "55555555-5555-5555-5555-555555555555";
        let malformed = "66666666-6666-6666-6666-666666666666";
        std::fs::write(fixture.path(fixture.date, empty), b"").unwrap();
        std::fs::write(fixture.path(fixture.date, malformed), b"{invalid}\n").unwrap();

        let rollouts = fixture.rollouts();
        for id in [CHILD, SECOND_CHILD] {
            // Mutate real child and foreign files after eligibility was sampled.
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(fixture.path(fixture.date, id))
                .unwrap();
            writeln!(file, "{}", codex_user_event(id, TURN)).unwrap();
        }
        let mut remaining = CODEX_READ_BUDGET;
        let rows = codex_rows(&rollouts, repo, TURN, &mut remaining)
            .unwrap()
            .0
            .rows;
        let mut expected = row(
            "codex",
            CHILD,
            "Native subagent",
            "",
            "2026-11-18T15:01:01+00:00",
            "",
            "unknown",
        );
        expected["started_at"] = json!("2026-11-18T15:01:01+00:00");
        assert_eq!(rows, [expected]);
        assert!(fixture.observe("abcdef1234567890abcd").is_none());
    }

    #[test]
    fn codex_rejects_ambiguous_assignment_and_wrong_event_owner() {
        let fixture = RolloutFixture::new("ambiguous-parent");
        let repo = fixture.store.repo();
        let main = codex_meta(PARENT, repo, json!("vscode"));
        fixture.write(
            fixture.date,
            PARENT,
            &[main.clone(), codex_user_event(CHILD, TURN)],
        );
        assert!(fixture.observe(TURN).is_none());

        let mut quoted = codex_user_event(PARENT, TURN);
        quoted["payload"]["item"]["content"][0]["text"] =
            json!(format!("Quoted assignment\n{}", codex_prompt(TURN)));
        fixture.write(fixture.date, PARENT, &[main.clone(), quoted]);
        assert!(fixture.observe(TURN).is_none());

        fixture.write(
            fixture.date,
            PARENT,
            &[main, codex_user_event(PARENT, TURN)],
        );
        fixture.write(
            fixture.date,
            CHILD,
            &[
                codex_meta(CHILD, repo, json!("vscode")),
                codex_user_event(CHILD, TURN),
            ],
        );
        assert!(fixture.observe(TURN).is_none());
        assert_eq!(codex_turn(&codex_prompt(TURN)), Some(TURN));
        assert_eq!(codex_turn(&codex_prompt("ABCDEF1234567890abcd")), None);
    }

    const NOW_MS: u64 = 1_800_000_000_000;
    const TURN_ID: &str = "019e-turn-1";

    fn codex_turn_event(turn_id: &str) -> Value {
        let mut event = codex_user_event(PARENT, TURN);
        event["payload"]["turn_id"] = json!(turn_id);
        event
    }

    fn codex_interaction(child: &str, started: Value, completed: Value) -> Value {
        json!({"type": "event_msg", "payload": {
            "type": "item_completed", "thread_id": PARENT, "turn_id": TURN_ID,
            "started_at_ms": started, "completed_at_ms": completed,
            "item": {"type": "SubAgentActivity", "kind": "interacted", "agent_thread_id": child}
        }})
    }

    fn interacted_ago(child: &str, seconds: u64) -> Value {
        let completed = NOW_MS - seconds * 1000;
        codex_interaction(child, json!(completed - 500), json!(completed))
    }

    /// Rows for one parent holding `events` after (or around) its turn record.
    fn seen_rows(label: &str, events: &[Value]) -> Vec<Value> {
        seen_rows_with(label, events, &[])
    }

    /// As `seen_rows`, plus extra child rollouts as `(id, parent_thread_id)`.
    fn seen_rows_with(label: &str, events: &[Value], extra: &[(&str, &str)]) -> Vec<Value> {
        let fixture = RolloutFixture::new(label);
        let repo = fixture.store.repo();
        let mut rows = vec![codex_meta(PARENT, repo, json!("vscode"))];
        rows.extend_from_slice(events);
        fixture.write(fixture.date, PARENT, &rows);
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        for (id, by) in extra {
            let header = codex_meta(
                id,
                repo,
                json!({"subagent": {"thread_spawn": {"parent_thread_id": by}}}),
            );
            fixture.write(fixture.date, id, &[header]);
        }
        let mut remaining = CODEX_READ_BUDGET;
        codex_rows_at(&fixture.rollouts(), repo, TURN, &mut remaining, NOW_MS)
            .unwrap()
            .0
            .rows
    }

    fn assert_seen(rows: &[Value], at: Option<u64>) {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "unknown");
        assert_eq!(rows[0].get("seen_at").and_then(Value::as_u64), at);
    }

    #[test]
    fn codex_marks_only_children_interacted_within_the_window_as_seen() {
        let turn_event = || codex_turn_event(TURN_ID);
        let turn = turn_event();
        let rows = seen_rows("seen-fresh", &[turn.clone(), interacted_ago(CHILD, 30)]);
        assert_seen(&rows, Some(NOW_MS - 30_000));
        assert_seen(
            &seen_rows("seen-edge", &[turn.clone(), interacted_ago(CHILD, 120)]),
            Some(NOW_MS - 120_000),
        );
        assert_seen(
            &seen_rows("seen-stale", &[turn.clone(), interacted_ago(CHILD, 121)]),
            None,
        );
        // A stub keeps exactly today's row.
        let stub = seen_rows("seen-stub", std::slice::from_ref(&turn));
        assert_eq!(
            stub,
            seen_rows("seen-stale-eq", &[turn.clone(), interacted_ago(CHILD, 500)])
        );

        // The latest valid interaction wins, whatever the record order.
        let rows = seen_rows(
            "seen-latest",
            &[
                turn.clone(),
                interacted_ago(CHILD, 10),
                interacted_ago(CHILD, 100),
                interacted_ago(CHILD, 600),
            ],
        );
        assert_seen(&rows, Some(NOW_MS - 10_000));
        // First-wins would keep the older record listed first.
        let rows = seen_rows(
            "seen-older-first",
            &[
                turn_event(),
                interacted_ago(CHILD, 100),
                interacted_ago(CHILD, 10),
            ],
        );
        assert_seen(&rows, Some(NOW_MS - 10_000));
        // A newer but invalid record cannot displace a valid older one.
        let future = codex_interaction(CHILD, json!(NOW_MS + 60_000), json!(NOW_MS + 61_000));
        let rows = seen_rows(
            "seen-invalid-newer",
            &[turn, interacted_ago(CHILD, 100), future],
        );
        assert_seen(&rows, Some(NOW_MS - 100_000));
    }

    #[test]
    fn codex_ignores_activity_that_is_not_an_exact_current_turn_interaction() {
        let turn = codex_turn_event(TURN_ID);
        let valid = interacted_ago(CHILD, 5);
        let mut cases: Vec<(&str, Value)> = Vec::new();
        let mut other_turn = valid.clone();
        other_turn["payload"]["turn_id"] = json!("another-turn");
        cases.push(("other-turn", other_turn));
        let mut other_parent = valid.clone();
        other_parent["payload"]["thread_id"] = json!(SECOND_CHILD);
        cases.push(("other-parent", other_parent));
        let mut kind = valid.clone();
        kind["payload"]["item"]["kind"] = json!("spawned");
        cases.push(("kind", kind));
        let mut item = valid.clone();
        item["payload"]["item"]["type"] = json!("SubAgentActivity2");
        cases.push(("item", item));
        let mut outer = valid.clone();
        outer["type"] = json!("response_item");
        cases.push(("outer", outer));
        let mut kinds = valid.clone();
        kinds["payload"]["type"] = json!("item_started");
        cases.push(("payload", kinds));
        cases.push(("unknown-child", interacted_ago(SECOND_CHILD, 5)));
        cases.push(("foreign-id", interacted_ago("not-a-thread-id", 5)));
        let at = NOW_MS - 5000;
        cases.push((
            "reversed",
            codex_interaction(CHILD, json!(at + 1), json!(at)),
        ));
        cases.push((
            "future",
            codex_interaction(CHILD, json!(NOW_MS + 5_001), json!(NOW_MS + 5_001)),
        ));
        cases.push((
            "pre-2020-started",
            codex_interaction(CHILD, json!(1_000), json!(at)),
        ));
        cases.push((
            "pre-2020",
            codex_interaction(CHILD, json!(1_000), json!(2_000)),
        ));
        for (name, value) in [
            ("float", json!(1.8e12)),
            ("string", json!(at.to_string())),
            ("negative", json!(-5)),
            ("null", Value::Null),
        ] {
            cases.push((name, codex_interaction(CHILD, json!(at), value.clone())));
            cases.push((name, codex_interaction(CHILD, value, json!(at))));
        }
        for (name, event) in cases {
            let rows = seen_rows(&format!("seen-bad-{name}"), &[turn.clone(), event]);
            assert_eq!(rows.len(), 1, "{name}");
            assert!(rows[0].get("seen_at").is_none(), "{name}");
        }
        // Skew inside the allowance is accepted.
        let skewed = codex_interaction(CHILD, json!(NOW_MS), json!(NOW_MS + 5_000));
        assert_seen(
            &seen_rows("seen-skew", &[turn.clone(), skewed]),
            Some(NOW_MS + 5_000),
        );

        // Activity before the UserMessage, or under an ambiguous/unusable turn id.
        assert_seen(
            &seen_rows("seen-before", &[valid.clone(), turn.clone()]),
            None,
        );
        assert_seen(
            &seen_rows(
                "seen-ambiguous",
                &[turn.clone(), valid.clone(), codex_turn_event("turn-2")],
            ),
            None,
        );
        assert_seen(
            &seen_rows(
                "seen-no-turn-id",
                &[codex_user_event(PARENT, TURN), valid.clone()],
            ),
            None,
        );
        assert_seen(
            &seen_rows(
                "seen-bad-turn-id",
                &[codex_turn_event("bad id"), valid.clone()],
            ),
            None,
        );
    }

    #[test]
    fn codex_seen_signal_requires_exactly_one_current_assignment() {
        let turn = codex_turn_event(TURN_ID);
        let valid = interacted_ago(CHILD, 5);
        assert_seen(
            &seen_rows("seen-once", &[turn.clone(), valid.clone()]),
            Some(NOW_MS - 5000),
        );
        for (name, events) in [
            ("same-id", vec![turn.clone(), turn.clone(), valid.clone()]),
            (
                "same-id-after",
                vec![turn.clone(), valid.clone(), turn.clone()],
            ),
            (
                "other-id",
                vec![codex_turn_event("turn-0"), turn.clone(), valid.clone()],
            ),
        ] {
            assert_seen(&seen_rows(&format!("seen-repeat-{name}"), &events), None);
        }
    }

    #[test]
    fn codex_seen_signal_skips_duplicate_children_and_keeps_collision_removal() {
        let fixture = RolloutFixture::new("seen-duplicate");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_turn_event(TURN_ID),
                interacted_ago(CHILD, 5),
                interacted_ago(SECOND_CHILD, 5),
            ],
        );
        for date in [fixture.date, fixture.date.pred_opt().unwrap()] {
            fixture.write(date, CHILD, &[codex_child_meta(CHILD, repo)]);
        }
        fixture.write(
            fixture.date,
            SECOND_CHILD,
            &[codex_child_meta(SECOND_CHILD, repo)],
        );
        let mut remaining = CODEX_READ_BUDGET;
        let observed = codex_rows_at(&fixture.rollouts(), repo, TURN, &mut remaining, NOW_MS)
            .unwrap()
            .0;
        let seen = |id: &str| {
            observed
                .rows
                .iter()
                .find(|row| row["participant"] == participant_id("codex", id))
                .unwrap()
        };
        assert!(seen(CHILD).get("seen_at").is_none());
        assert!(!observed.codex.contains_key(&participant_id("codex", CHILD)));
        assert_eq!(seen(SECOND_CHILD)["seen_at"], NOW_MS - 5000);
        assert!(
            observed
                .codex
                .contains_key(&participant_id("codex", SECOND_CHILD))
        );
    }

    #[test]
    fn codex_seen_signal_uses_only_complete_sampled_records() {
        let line = |event: &Value| format!("{event}\n");
        let seen = |body: &str, recent_only| {
            codex_turn_activity(body.as_bytes(), recent_only, PARENT, TURN, NOW_MS).interactions
        };
        let whole = [
            line(&codex_turn_event(TURN_ID)),
            line(&interacted_ago(CHILD, 5)),
        ]
        .concat();
        assert_eq!(seen(&whole, false)[CHILD], NOW_MS - 5000);
        // A suffix sample starts mid-record: its first line is never trusted.
        assert!(seen(&whole, true).is_empty());
        let suffixed = format!("{}{whole}", &line(&interacted_ago(CHILD, 1))[10..]);
        assert_eq!(seen(&suffixed, true)[CHILD], NOW_MS - 5000);
        // A torn final record has no LF and is not a record.
        let torn = line(&interacted_ago(CHILD, 1));
        let torn = format!("{whole}{}", &torn[..torn.len() - 1]);
        assert_eq!(seen(&torn, false)[CHILD], NOW_MS - 5000);
    }

    #[test]
    fn codex_child_recency_adds_no_reads() {
        let measure = |label: &str, events: &[Value]| {
            let fixture = RolloutFixture::new(label);
            let repo = fixture.store.repo();
            let mut rows = vec![codex_meta(PARENT, repo, json!("vscode"))];
            rows.extend_from_slice(events);
            fixture.write(fixture.date, PARENT, &rows);
            let mut child = codex_child_meta(CHILD, repo);
            child["payload"]["timestamp"] = json!(codex_fixture_time(NOW_MS - 5_000));
            fixture.write(fixture.date, CHILD, &[child]);
            let size = std::fs::metadata(fixture.path(fixture.date, PARENT))
                .unwrap()
                .len();
            let mut remaining = CODEX_READ_BUDGET;
            let rows = codex_rows_at(&fixture.rollouts(), repo, TURN, &mut remaining, NOW_MS)
                .unwrap()
                .0
                .rows;
            (CODEX_READ_BUDGET - remaining, size, rows)
        };
        // Baseline: the same rollout set with no SubAgentActivity record at all,
        // padded to the same byte size.
        let fresh = interacted_ago(CHILD, 5);
        let pad = json!({"pad": "x".repeat(fresh.to_string().len() - 10)});
        assert_eq!(pad.to_string().len(), fresh.to_string().len());
        let (base, base_size, rows) = measure("reads-a", &[codex_turn_event(TURN_ID), pad]);
        assert!(rows[0].get("seen_at").is_none());
        let (stale, stale_size, rows) = measure(
            "reads-b",
            &[codex_turn_event(TURN_ID), interacted_ago(CHILD, 130)],
        );
        assert!(rows[0].get("seen_at").is_none());
        let (used, size, rows) = measure("reads-c", &[codex_turn_event(TURN_ID), fresh]);
        assert_eq!(rows[0]["seen_at"], NOW_MS - 5000);
        assert_eq!((used, size), (base, base_size));
        assert_eq!((stale, stale_size), (base, base_size));

        let old = codex_completed_turn(NOW_MS - 1_000);
        let recent = codex_completed_turn(NOW_MS - 10_000);
        let (hidden, hidden_size, rows) = measure("created-reads-a", &[old]);
        assert!(rows[0].get("codex_recent_creation").is_none());
        let (shown, shown_size, rows) = measure("created-reads-b", &[recent]);
        assert!(rows[0]["codex_recent_creation"].is_object());
        assert_eq!((hidden, hidden_size), (shown, shown_size));
    }

    fn codex_fixture_time(ms: u64) -> String {
        chrono::DateTime::from_timestamp_millis(ms.try_into().unwrap())
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn codex_completed_turn(at: u64) -> Value {
        let mut turn = codex_turn_event(TURN_ID);
        turn["payload"]["completed_at_ms"] = json!(at);
        turn
    }

    const SIBLINGS: [&str; 5] = [
        "33333333-3333-3333-3333-333333333333",
        "44444444-4444-4444-4444-444444444444",
        "55555555-5555-5555-5555-555555555555",
        "66666666-6666-6666-6666-666666666666",
        "77777777-7777-7777-7777-777777777777",
    ];

    /// One parent with `children` in header order (newest mtime first); each
    /// child answers "reply <id>".
    fn sibling_fixture(label: &str, events: &[Value], children: &[&str]) -> RolloutFixture {
        let fixture = RolloutFixture::new(label);
        let repo = fixture.store.repo();
        let mut rows = vec![codex_meta(PARENT, repo, json!("vscode"))];
        rows.extend_from_slice(events);
        fixture.write(fixture.date, PARENT, &rows);
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (age, id) in children.iter().enumerate() {
            let reply = codex_agent_event(id, "reply", &format!("reply {id}"));
            let body = [
                codex_child_meta(id, repo),
                codex_context("reply", repo),
                reply,
            ];
            fixture.write(fixture.date, id, &body);
            File::options()
                .write(true)
                .open(fixture.path(fixture.date, id))
                .unwrap()
                .set_modified(base - std::time::Duration::from_secs(age as u64))
                .unwrap();
        }
        fixture
    }

    /// As `sibling_fixture`, observed at each of `nows`.
    fn sibling_projections(
        label: &str,
        events: &[Value],
        children: &[&str],
        nows: &[u64],
    ) -> Vec<ObservedChildren> {
        let fixture = sibling_fixture(label, events, children);
        nows.iter()
            .map(|&now| fixture.projection_at(TURN, now).unwrap())
            .collect()
    }

    fn sibling_projection(label: &str, events: &[Value], children: &[&str]) -> ObservedChildren {
        sibling_projections(label, events, children, &[NOW_MS])
            .pop()
            .unwrap()
    }

    fn selected(observed: &ObservedChildren) -> Vec<(String, Option<u64>)> {
        observed
            .rows
            .iter()
            .map(|row| {
                assert_eq!(row["state"], "unknown");
                (
                    text(row, "participant").to_owned(),
                    row.get("seen_at").and_then(Value::as_u64),
                )
            })
            .collect()
    }

    fn sibling(index: usize, seen: Option<u64>) -> (String, Option<u64>) {
        let seen = seen.map(|seconds| NOW_MS - seconds * 1000);
        (participant_id("codex", SIBLINGS[index]), seen)
    }

    #[test]
    fn codex_recent_creation_promotes_an_exact_child_without_an_interaction() {
        let turn = codex_completed_turn(NOW_MS - 60_000);
        let interaction = interacted_ago(SIBLINGS[2], 30);
        let fixture = sibling_fixture(
            "recent-creation",
            &[turn.clone(), interaction.clone()],
            &SIBLINGS[..4],
        );
        let repo = fixture.store.repo();
        let new = SIBLINGS[3];
        let participant = participant_id("codex", new);
        let newest = NOW_MS - 5_000;
        let path = fixture.path(fixture.date, new);
        let prior_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let write_child = |date, created, parent| {
            let mut header = codex_child_meta(new, repo);
            // A fresh envelope is common when earlier metadata is replayed.
            header["timestamp"] = json!(codex_fixture_time(newest));
            header["payload"]["timestamp"] = created;
            header["payload"]["source"]["subagent"]["thread_spawn"]["parent_thread_id"] =
                json!(parent);
            fixture.write(
                date,
                new,
                &[
                    header,
                    codex_context("reply", repo),
                    codex_agent_event(new, "reply", &format!("reply {new}")),
                ],
            );
            File::options()
                .write(true)
                .open(fixture.path(date, new))
                .unwrap()
                .set_modified(prior_mtime)
                .unwrap();
        };
        let observe = || fixture.projection_at(TURN, NOW_MS).unwrap();
        let folded = [sibling(2, Some(30)), sibling(0, None), sibling(1, None)];
        for created in [
            json!(codex_fixture_time(NOW_MS - 80_000)),
            json!(codex_fixture_time(NOW_MS + 5_001)),
            json!("bad clock"),
            Value::Null,
        ] {
            write_child(fixture.date, created, PARENT);
            assert_eq!(selected(&observe()), folded);
        }
        let created = json!(codex_fixture_time(newest));
        write_child(fixture.date, created.clone(), CHILD);
        assert_eq!(selected(&observe()), folded);
        write_child(fixture.date, created.clone(), PARENT);
        let admitted = observe();
        assert_eq!(
            selected(&admitted),
            [sibling(2, Some(30)), sibling(3, None), sibling(0, None)]
        );
        assert_eq!(
            admitted.rows[1]["codex_recent_creation"],
            json!({"at_ms": newest, "observed_ms": NOW_MS})
        );
        assert_eq!(
            fixture.chat(
                &format!("› {}", codex_prompt(TURN)),
                &admitted.codex[&participant],
            ),
            Some(format!("reply {new}"))
        );
        let expired = fixture.projection_at(TURN, newest + 120_001).unwrap();
        assert_eq!(
            selected(&expired),
            [sibling(0, None), sibling(1, None), sibling(2, None)]
        );

        let yesterday = fixture.date.pred_opt().unwrap();
        write_child(yesterday, created, PARENT);
        assert_eq!(selected(&observe()), folded);
        std::fs::remove_file(fixture.path(yesterday, new)).unwrap();
        let ordinary_text = "Please review the other module";
        let mut ordinary = codex_user_event(PARENT, TURN);
        ordinary["payload"]["turn_id"] = json!("next-turn");
        ordinary["payload"]["completed_at_ms"] = json!(NOW_MS - 2_000);
        ordinary["payload"]["item"]["content"][0]["text"] = json!(ordinary_text);
        let mut foreign = ordinary.clone();
        foreign["payload"]["thread_id"] = json!(CHILD);
        let viewport = format!("› {}\n› {ordinary_text}", codex_prompt(TURN));
        assert_eq!(codex_current_turn(&viewport), Some(TURN));
        for (events, expected) in [
            (
                vec![ordinary.clone(), turn.clone(), interaction.clone(), foreign],
                selected(&admitted),
            ),
            (vec![turn.clone(), ordinary, interaction], folded.to_vec()),
        ] {
            let mut parent = vec![codex_meta(PARENT, repo, json!("vscode"))];
            parent.extend(events);
            fixture.write(fixture.date, PARENT, &parent);
            assert_eq!(selected(&observe()), expected);
        }
        for turns in [vec![codex_turn_event(TURN_ID)], vec![turn.clone(), turn]] {
            let mut events = vec![codex_meta(PARENT, repo, json!("vscode"))];
            events.extend(turns);
            fixture.write(fixture.date, PARENT, &events);
            assert_eq!(
                selected(&observe()),
                [sibling(0, None), sibling(1, None), sibling(2, None)]
            );
        }
    }

    #[test]
    fn codex_selects_a_recently_seen_older_sibling_before_header_order() {
        let turn = codex_turn_event(TURN_ID);
        let four = &SIBLINGS[..4];
        let history = sibling_projection("seen-history", std::slice::from_ref(&turn), four);
        assert_eq!(
            selected(&history),
            [sibling(0, None), sibling(1, None), sibling(2, None)]
        );
        let older = interacted_ago(SIBLINGS[3], 30);
        // The same rollouts once the interaction has aged out of the window.
        let [observed, aged] = sibling_projections(
            "seen-older",
            &[turn, older],
            four,
            &[NOW_MS, NOW_MS + 91_000],
        )
        .try_into()
        .ok()
        .unwrap();
        assert_eq!(
            selected(&observed),
            [sibling(3, Some(30)), sibling(0, None), sibling(1, None)]
        );
        assert_eq!(selected(&aged), selected(&history));
        let history = aged;
        // Children kept in both selections keep an equal direct-chat locator
        // (and chat binding) although the observation clocks differ; the
        // promoted one is anchored to its own rollout under the same parent.
        use crate::orchestrator::NativeLocator;
        for index in [0, 1] {
            let id = participant_id("codex", SIBLINGS[index]);
            let (now, aged) = (&observed.codex[&id], &history.codex[&id]);
            assert_eq!(
                (now.observed_ms, aged.observed_ms),
                (NOW_MS, NOW_MS + 91_000)
            );
            assert!(now == aged, "{index}");
            assert!(NativeLocator::Codex(now.clone()) == NativeLocator::Codex(aged.clone()));
        }
        // Any other child, anchor or turn is a different locator.
        let first = &observed.codex[&participant_id("codex", SIBLINGS[0])];
        assert!(*first != observed.codex[&participant_id("codex", SIBLINGS[1])]);
        let mut changed = first.clone();
        changed.turn = STALE_TURN.into();
        assert!(changed != *first);
        let mut changed = first.clone();
        changed.child.inode += 1;
        assert!(changed != *first);
        let mut changed = first.clone();
        changed.parent.path = changed.child.path.clone();
        assert!(changed != *first);
        let mut changed = first.clone();
        changed.root.1 += 1;
        assert!(changed != *first);
        let promoted = &observed.codex[&participant_id("codex", SIBLINGS[3])];
        assert_eq!(promoted.child.id, SIBLINGS[3]);
        let name = promoted.child.path.file_name().unwrap().to_str().unwrap();
        assert!(name.ends_with(&format!("-{}.jsonl", SIBLINGS[3])), "{name}");
        assert!(promoted.parent == history.codex[&sibling(0, None).0].parent);
        assert_eq!(promoted.turn, TURN);
    }

    #[test]
    fn codex_keeps_only_the_three_most_recent_seen_children() {
        let turn = codex_turn_event(TURN_ID);
        let mut events = vec![turn.clone()];
        for (id, ago) in SIBLINGS.iter().zip([100, 90, 10, 20, 30]) {
            events.push(interacted_ago(id, ago));
        }
        assert_eq!(
            selected(&sibling_projection("seen-five", &events, &SIBLINGS)),
            [
                sibling(2, Some(10)),
                sibling(3, Some(20)),
                sibling(4, Some(30))
            ]
        );
        // Equal stamps keep header order.
        let mut tied = vec![turn.clone()];
        tied.extend(SIBLINGS.iter().map(|id| interacted_ago(id, 7)));
        assert_eq!(
            selected(&sibling_projection("seen-tied", &tied, &SIBLINGS)),
            [
                sibling(0, Some(7)),
                sibling(1, Some(7)),
                sibling(2, Some(7))
            ]
        );
        // Stale and future observations claim no slot; history fills in order.
        let stale = interacted_ago(SIBLINGS[3], 121);
        let future = codex_interaction(SIBLINGS[4], json!(NOW_MS), json!(NOW_MS + 5_001));
        assert_eq!(
            selected(&sibling_projection(
                "seen-stale-future",
                &[turn.clone(), stale, future],
                &SIBLINGS
            )),
            [sibling(0, None), sibling(1, None), sibling(2, None)]
        );
        // Wrong parent or wrong turn claims no slot either.
        let mut other_parent = interacted_ago(SIBLINGS[3], 5);
        other_parent["payload"]["thread_id"] = json!(SIBLINGS[0]);
        let mut other_turn = interacted_ago(SIBLINGS[4], 5);
        other_turn["payload"]["turn_id"] = json!("another-turn");
        assert_eq!(
            selected(&sibling_projection(
                "seen-foreign-turn",
                &[turn, other_parent, other_turn],
                &SIBLINGS
            )),
            [sibling(0, None), sibling(1, None), sibling(2, None)]
        );
    }

    #[test]
    fn codex_skips_a_duplicated_child_when_choosing_recent_slots() {
        let duplicate = SIBLINGS[4];
        let mut events = vec![codex_turn_event(TURN_ID), interacted_ago(duplicate, 5)];
        for (id, ago) in SIBLINGS.iter().zip([10, 20, 30]) {
            events.push(interacted_ago(id, ago));
        }
        let fixture = sibling_fixture("seen-duplicate-slot", &events, &SIBLINGS);
        let older = fixture.date.pred_opt().unwrap();
        let repo = fixture.store.repo();
        fixture.write(older, duplicate, &[codex_child_meta(duplicate, repo)]);
        let observed = fixture.projection_at(TURN, NOW_MS).unwrap();
        assert_eq!(
            selected(&observed),
            [
                sibling(0, Some(10)),
                sibling(1, Some(20)),
                sibling(2, Some(30))
            ]
        );
        assert!(
            !observed
                .codex
                .contains_key(&participant_id("codex", duplicate))
        );
    }

    #[test]
    fn codex_chat_reselects_children_at_the_observation_clock() {
        use std::io::Write;

        // The observation happened an hour before the chat request.
        let observed_at = codex_now_ms() - 3_600_000;
        let at = |offset: u64| json!(observed_at - offset);
        let four = &SIBLINGS[..4];
        let terminal = format!("› {}", codex_prompt(TURN));
        let promoted = participant_id("codex", SIBLINGS[3]);

        // A promoted older sibling still opens after its observation aged out.
        let fixture = sibling_fixture(
            "chat-promoted",
            &[
                codex_turn_event(TURN_ID),
                codex_interaction(SIBLINGS[3], at(31_000), at(30_000)),
            ],
            four,
        );
        let observed = fixture.projection_at(TURN, observed_at).unwrap();
        assert_eq!(
            observed.rows[0]["participant"].as_str(),
            Some(promoted.as_str())
        );
        let locator = &observed.codex[&promoted];
        assert_eq!(locator.observed_ms, observed_at);
        let reply = format!("reply {}", SIBLINGS[3]);
        assert_eq!(fixture.chat(&terminal, locator), Some(reply.clone()));
        // A child outside the observed selection is refused at that clock.
        let mut unshown = fixture
            .projection_at(TURN, observed_at + 200_000)
            .unwrap()
            .codex
            .remove(&participant_id("codex", SIBLINGS[2]))
            .unwrap();
        assert!(fixture.chat(&terminal, &unshown).is_some());
        unshown.observed_ms = observed_at;
        assert!(fixture.chat(&terminal, &unshown).is_none());
        // A replaced rollout (a new inode while the old one is live) breaks
        // the exact anchor.
        let path = fixture.path(fixture.date, SIBLINGS[3]);
        let staged = path.with_extension("staged");
        std::fs::copy(&path, &staged).unwrap();
        std::fs::rename(&staged, &path).unwrap();
        assert!(fixture.chat(&terminal, locator).is_none());

        // A sibling seen only after the observation cannot displace a history row.
        let fixture = sibling_fixture("chat-history", &[codex_turn_event(TURN_ID)], four);
        let observed = fixture.projection_at(TURN, observed_at).unwrap();
        let history = participant_id("codex", SIBLINGS[2]);
        assert!(
            observed
                .rows
                .iter()
                .any(|row| row["participant"] == history)
        );
        let late = codex_now_ms() - 1_000;
        let mut parent = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        writeln!(
            parent,
            "{}",
            codex_interaction(SIBLINGS[3], json!(late - 500), json!(late))
        )
        .unwrap();
        assert_eq!(
            fixture.chat(&terminal, &observed.codex[&history]),
            Some(format!("reply {}", SIBLINGS[2]))
        );
    }

    #[test]
    fn codex_seen_signal_requires_a_child_of_this_parent() {
        const FOREIGN: &str = "77777777-7777-7777-7777-777777777777";
        const OTHER_PARENT: &str = "88888888-8888-8888-8888-888888888888";
        let rows = seen_rows_with(
            "seen-foreign",
            &[
                codex_turn_event(TURN_ID),
                interacted_ago(CHILD, 5),
                interacted_ago(FOREIGN, 5),
            ],
            &[(FOREIGN, OTHER_PARENT)],
        );
        assert_seen(&rows, Some(NOW_MS - 5000));
        assert_eq!(rows[0]["participant"], participant_id("codex", CHILD));
    }

    #[test]
    fn codex_interactions_only_names_uuid_threads() {
        let body = |child: &str| {
            format!(
                "{}\n{}\n",
                codex_turn_event(TURN_ID),
                interacted_ago(child, 5)
            )
        };
        let seen = |child: &str| {
            codex_turn_activity(body(child).as_bytes(), false, PARENT, TURN, NOW_MS).interactions
        };
        assert_eq!(seen(CHILD).len(), 1);
        for bad in [
            "not-a-thread-id",
            "1111111111111111111111111111111111111",
            "",
        ] {
            assert!(seen(bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn codex_seen_signal_ignores_the_partial_first_line_of_a_large_parent() {
        use std::io::Write;

        let fixture = RolloutFixture::new("seen-large");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[codex_meta(PARENT, repo, json!("vscode"))],
        );
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        // The sampled suffix begins mid-file: its first line, a complete record
        // with a second turn id, must not be trusted (else the turn is ambiguous).
        let mut tail = format!(
            "{}\n{}\n{}\n",
            codex_turn_event("stale-turn"),
            codex_turn_event(TURN_ID),
            interacted_ago(CHILD, 5)
        );
        let fill = CODEX_RECENT_ROOT_BYTES + 1 - tail.len();
        tail.push_str(&" ".repeat(fill - 1));
        tail.push('\n');
        assert_eq!(tail.len(), CODEX_RECENT_ROOT_BYTES + 1);
        let mut parent = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        parent
            .seek(SeekFrom::Start(MAX_TRANSCRIPT as u64 + 4096))
            .unwrap();
        parent.write_all(tail.as_bytes()).unwrap();
        let mut remaining = CODEX_READ_BUDGET;
        let rows = codex_rows_at(&fixture.rollouts(), repo, TURN, &mut remaining, NOW_MS)
            .unwrap()
            .0
            .rows;
        assert_seen(&rows, Some(NOW_MS - 5000));
    }

    #[test]
    fn codex_seen_signal_trusts_every_record_of_a_parent_at_the_size_cap() {
        let fixture = RolloutFixture::new("seen-exact");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_turn_event(TURN_ID),
                interacted_ago(CHILD, 5),
            ],
        );
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap()
            .set_len(MAX_TRANSCRIPT as u64)
            .unwrap();
        let mut remaining = CODEX_READ_BUDGET;
        let rollouts = fixture.rollouts();
        // Exactly at the cap the whole body is read, so its first record counts.
        let rows = codex_rows_at(&rollouts, repo, TURN, &mut remaining, NOW_MS)
            .unwrap()
            .0
            .rows;
        assert_seen(&rows, Some(NOW_MS - 5000));
    }

    #[test]
    fn codex_ignores_replaced_and_symlinked_rollouts() {
        let fixture = RolloutFixture::new("replaced-rollout");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_user_event(PARENT, TURN),
            ],
        );
        fixture.write(
            fixture.date,
            CHILD,
            &[
                codex_child_meta(CHILD, repo),
                codex_context("chat", repo),
                codex_agent_event(CHILD, "chat", "Original exact answer"),
            ],
        );
        let projection = fixture.projection(TURN).unwrap();
        let locator = &projection.codex[text(&projection.rows[0], "participant")];
        let terminal = format!("› {}", codex_prompt(TURN));
        assert_eq!(
            fixture.chat(&terminal, locator).as_deref(),
            Some("Original exact answer")
        );
        let path = fixture.path(fixture.date, CHILD);
        let rollouts = fixture.rollouts();
        let sampled = rollouts
            .candidates
            .iter()
            .find(|candidate| codex_filename_id(&candidate.path) == Some(CHILD))
            .unwrap();
        let (pinned, _) = rollouts.open(sampled).unwrap();
        let replacement = path.with_extension("replacement");
        std::fs::copy(&path, &replacement).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(rollouts.status(sampled, &pinned).is_none());
        assert!(rollouts.open(sampled).is_none());
        let mut remaining = CODEX_READ_BUDGET;
        assert!(codex_rows(&rollouts, repo, TURN, &mut remaining).is_none());
        assert!(fixture.chat(&terminal, locator).is_none());

        std::fs::rename(&path, &replacement).unwrap();
        assert!(fixture.chat(&terminal, locator).is_none());
        std::os::unix::fs::symlink(&replacement, &path).unwrap();
        assert!(fixture.observe(TURN).unwrap().is_empty());
        assert!(fixture.chat(&terminal, locator).is_none());
    }

    #[test]
    fn codex_verifies_only_the_sampled_root_bytes_after_growth() {
        use std::io::Write;

        let fixture = RolloutFixture::new("growing-rollout");
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, fixture.store.repo(), json!("vscode")),
                codex_user_event(PARENT, TURN),
            ],
        );
        let rollouts = fixture.rollouts();
        let rollout = &rollouts.candidates[0];
        let (mut file, sampled) = rollouts.open(rollout).unwrap();
        let size = sampled.st_size as usize;
        let mut remaining = CODEX_READ_BUDGET;
        let header = codex_read_header(&mut file, size as u64, &mut remaining).unwrap();
        assert_eq!(CODEX_READ_BUDGET - remaining, size); // Header read-ahead consumed the short body.
        let offset = header.len() as u64;
        let body = codex_read_range(&mut file, offset, size - header.len(), &mut remaining)
            .unwrap()
            .unwrap();

        let path = fixture.path(fixture.date, PARENT);
        let mut appender = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(appender, "{}", codex_user_event(PARENT, STALE_TURN)).unwrap();
        let ranges = [(0, header.as_slice()), (offset, body.as_slice())];
        assert_eq!(
            rollouts.confirm_snapshot(rollout, &mut file, &sampled, &ranges, &mut remaining),
            Some(true)
        );
        assert_eq!(CODEX_READ_BUDGET - remaining, 3 * size - header.len());
        assert!(codex_has_assignment(&body, false, PARENT, TURN));
        assert!(!codex_has_assignment(&body, false, PARENT, STALE_TURN));

        let mut insufficient = size - 1;
        assert_eq!(
            rollouts.confirm_snapshot(rollout, &mut file, &sampled, &ranges, &mut insufficient),
            None
        );

        let mut editor = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        editor.seek(SeekFrom::Start(offset)).unwrap();
        editor.write_all(b"x").unwrap();
        assert_eq!(
            rollouts.confirm_snapshot(rollout, &mut file, &sampled, &ranges, &mut remaining),
            Some(false)
        );
    }

    #[test]
    fn codex_reads_late_assignments_within_shared_body_budget() {
        use std::io::Write;

        let fixture = RolloutFixture::new("assignment-budget");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[codex_meta(PARENT, repo, json!("vscode"))],
        );
        let mut parent = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        parent.seek(SeekFrom::Start(2 * 1024 * 1024)).unwrap();
        writeln!(parent, "\n{}", codex_user_event(PARENT, TURN)).unwrap();
        assert!(fixture.observe(TURN).is_some());

        for id in [CHILD, SECOND_CHILD] {
            fixture.write(fixture.date, id, &[codex_meta(id, repo, json!("vscode"))]);
            let other = std::fs::OpenOptions::new()
                .write(true)
                .open(fixture.path(fixture.date, id))
                .unwrap();
            other.set_len(MAX_TRANSCRIPT as u64).unwrap();
        }
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_projects_children_of_a_long_root_after_a_complete_recent_turn() {
        use std::io::Write;

        let fixture = RolloutFixture::new("long-parent");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_user_event(PARENT, STALE_TURN),
            ],
        );
        fixture.write(
            fixture.date,
            CHILD,
            &[codex_meta(
                CHILD,
                repo,
                json!({"subagent": {"thread_spawn": {"parent_thread_id": PARENT}}}),
            )],
        );
        let mut parent = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        parent.seek(SeekFrom::Start(7 * 1024 * 1024)).unwrap();
        write!(parent, "\n{}", codex_user_event(PARENT, TURN)).unwrap();
        assert!(fixture.observe(TURN).is_none());
        parent.write_all(b"\n").unwrap();

        let rows = fixture.observe(TURN).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["participant"],
            format!(
                "native-codex-{}",
                &oms_panel_core::digest(CHILD.as_bytes())[..16]
            )
        );
        assert!(fixture.observe(STALE_TURN).is_none());

        fixture.write(
            fixture.date,
            SECOND_CHILD,
            &[
                codex_meta(SECOND_CHILD, repo, json!("vscode")),
                codex_user_event(SECOND_CHILD, TURN),
            ],
        );
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_tail_requires_a_complete_bounded_record_at_its_boundary() {
        use std::io::Write;

        let fixture = RolloutFixture::new("long-parent-boundary");
        fixture.write(
            fixture.date,
            PARENT,
            &[codex_meta(PARENT, fixture.store.repo(), json!("vscode"))],
        );
        let mut parent = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.path(fixture.date, PARENT))
            .unwrap();
        let boundary = MAX_TRANSCRIPT as u64 - 1;
        parent
            .set_len(boundary + CODEX_RECENT_ROOT_BYTES as u64 + 1)
            .unwrap();
        parent.seek(SeekFrom::Start(boundary)).unwrap();
        let mut event = codex_user_event(PARENT, TURN);
        writeln!(parent, "\n{event}").unwrap();
        assert!(fixture.observe(TURN).is_some());

        parent.seek(SeekFrom::Start(boundary)).unwrap();
        parent.write_all(b"x").unwrap();
        assert!(fixture.observe(TURN).is_none());

        event["padding"] = json!("x".repeat(MAX_LINE));
        parent.seek(SeekFrom::Start(boundary)).unwrap();
        writeln!(parent, "\n{event}").unwrap();
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_displays_only_safe_labels_from_bound_spawn_metadata() {
        let fixture = RolloutFixture::new("codex-label");
        let repo = fixture.store.repo();
        fixture.write(
            fixture.date,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("vscode")),
                codex_user_event(PARENT, TURN),
            ],
        );
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        let third = "33333333-3333-3333-3333-333333333333";
        for (id, nickname) in [
            (CHILD, "Ada"),
            (SECOND_CHILD, sample.as_str()),
            (third, "\x1b[31mColour"),
        ] {
            let mut meta = codex_meta(
                id,
                repo,
                json!({
                    "subagent": {"thread_spawn": {"parent_thread_id": PARENT, "agent_nickname": nickname}},
                }),
            );
            meta["payload"]["title"] = json!("Raw runtime title");
            meta["payload"]["agent_nickname"] = json!("Unbound nickname");
            fixture.write(fixture.date, id, &[meta]);
        }
        let rows = fixture.observe(TURN).unwrap();
        let titles: BTreeSet<_> = rows.iter().map(|row| text(row, "title").trim()).collect();
        assert_eq!(
            titles,
            BTreeSet::from(["Ada", "[oms-redacted:github-token]", "Native subagent"])
        );
    }

    #[test]
    fn codex_discovers_older_verified_vscode_and_cli_parent_for_new_child() {
        let older = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let terminal = format!("› {}\n", codex_prompt(TURN));
        for source in ["vscode", "cli"] {
            let fixture = RolloutFixture::new(&format!("older-parent-{source}"));
            let repo = fixture.store.repo();
            fixture.write(
                older,
                PARENT,
                &[
                    codex_meta(PARENT, repo, json!(source)),
                    codex_user_event(PARENT, TURN),
                ],
            );
            fixture.write(
                fixture.date,
                CHILD,
                &[
                    codex_child_meta(CHILD, repo),
                    codex_context("reply-turn", repo),
                    codex_agent_event(CHILD, "reply-turn", "older parent reply"),
                ],
            );
            let observed = fixture.projection(TURN).unwrap();
            let participant = participant_id("codex", CHILD);
            assert_eq!(observed.rows.len(), 1);
            assert_eq!(text(&observed.rows[0], "participant"), participant);
            let locator = observed.codex.get(&participant).unwrap();
            assert_eq!(
                fixture.chat(&terminal, locator).as_deref(),
                Some("older parent reply")
            );
        }
    }

    #[test]
    fn codex_rejects_older_parent_with_wrong_scope_or_source() {
        let older = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let fixture = RolloutFixture::new("older-parent-wrong-scope");
        let repo = fixture.store.repo();
        let other_repo = repo.join("other");
        std::fs::create_dir(&other_repo).unwrap();
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);

        // Wrong repo/cwd.
        fixture.write(
            older,
            PARENT,
            &[
                codex_meta(PARENT, &other_repo, json!("cli")),
                codex_user_event(PARENT, TURN),
            ],
        );
        assert!(fixture.observe(TURN).is_none());

        // Wrong thread_source (not "user").
        let mut non_user = codex_meta(PARENT, repo, json!("cli"));
        non_user["payload"]["thread_source"] = json!("subagent");
        fixture.write(older, PARENT, &[non_user, codex_user_event(PARENT, TURN)]);
        assert!(fixture.observe(TURN).is_none());

        // Unsupported source.
        fixture.write(
            older,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("exec")),
                codex_user_event(PARENT, TURN),
            ],
        );
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_rejects_duplicate_older_parent_files() {
        let older = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let earlier = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        let fixture = RolloutFixture::new("older-parent-duplicate");
        let repo = fixture.store.repo();
        let parent_rows = [
            codex_meta(PARENT, repo, json!("cli")),
            codex_user_event(PARENT, TURN),
        ];
        fixture.write(older, PARENT, &parent_rows);
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        assert!(fixture.observe(TURN).is_some());

        // Duplicate matching the same parent_thread_id in the same older date directory.
        let same_day_dup = fixture
            .path(older, PARENT)
            .with_file_name(format!("rollout-2026-11-13T16-02-02-{PARENT}.jsonl"));
        std::fs::copy(fixture.path(older, PARENT), &same_day_dup).unwrap();
        assert!(fixture.observe(TURN).is_none());
        std::fs::remove_file(&same_day_dup).unwrap();
        assert!(fixture.observe(TURN).is_some());

        // Duplicate matching the same parent_thread_id in another older date directory.
        fixture.write(earlier, PARENT, &parent_rows);
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_bounds_older_parent_lookback_budget_and_stop() {
        let too_old = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let edge = chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let fixture = RolloutFixture::new("older-parent-bounds");
        let repo = fixture.store.repo();
        let parent_rows = [
            codex_meta(PARENT, repo, json!("cli")),
            codex_user_event(PARENT, TURN),
        ];
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);

        // Beyond the 14-day lookback cap (today - 16): not discovered.
        fixture.write(too_old, PARENT, &parent_rows);
        assert!(fixture.observe(TURN).is_none());

        // At the exact 14-day lookback boundary (today - 15): discovered without scanning too_old.
        fixture.write(edge, PARENT, &parent_rows);
        assert!(fixture.observe(TURN).is_some());

        // Exhausted remaining budget after reading the child header stops boundedly.
        let rollouts = fixture.rollouts();
        let mut tight = std::fs::read(fixture.path(fixture.date, CHILD))
            .unwrap()
            .len();
        assert!(codex_rows(&rollouts, repo, TURN, &mut tight).is_none());
        assert_eq!(tight, 0);

        // Pre-set stop flag aborts without consuming read budget.
        let mut remaining = CODEX_READ_BUDGET;
        assert!(
            codex_rows_checked(
                &rollouts,
                repo,
                TURN,
                &mut remaining,
                &AtomicBool::new(true),
                0,
                true,
            )
            .is_none()
        );
        assert_eq!(remaining, CODEX_READ_BUDGET);
    }

    fn codex_uuidv7(date: chrono::NaiveDate, suffix: &str) -> String {
        let ms = u64::try_from(
            date.and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
        )
        .unwrap();
        let hi = (ms >> 16) as u32;
        let lo = (ms & 0xffff) as u16;
        format!("{hi:08x}-{lo:04x}-7000-8000-{suffix:0>12}")
    }

    fn write_codex_decoys(repo: &Path, date: chrono::NaiveDate, count: usize) {
        let dir = repo
            .join("sessions")
            .join(date.format("%Y/%m/%d").to_string());
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..count {
            std::fs::File::create(dir.join(format!("d{i}"))).unwrap();
        }
    }

    #[test]
    fn codex_uuidv7_hint_skips_unhinted_days_and_honors_timezone_margin() {
        let hint_day = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let tz_prev = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        let tz_next = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let decoy_day_1 = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let decoy_day_2 = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let ancient_day = chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let edge_day = chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();

        let p_exact = codex_uuidv7(hint_day, "1");
        assert_eq!(codex_uuidv7_date(&p_exact), Some(hint_day));
        assert_eq!(codex_uuidv7_date(PARENT), None);
        assert_eq!(codex_uuidv7_date(&p_exact.to_ascii_uppercase()), None);
        let bad_variant = format!("{}-c000-{}", &p_exact[..18], &p_exact[24..]);
        assert_eq!(codex_uuidv7_date(&bad_variant), None);

        let fixture = RolloutFixture::new("older-parent-uuidv7-hint");
        let repo = fixture.store.repo();
        // More than CODEX_OLDER_PARENT_ENTRY_BUDGET decoys in unhinted days (today - 2, today - 3).
        write_codex_decoys(repo, decoy_day_1, 1100);
        write_codex_decoys(repo, decoy_day_2, 1100);

        let write_child = |parent_id: &str| {
            fixture.write(
                fixture.date,
                CHILD,
                &[codex_meta(
                    CHILD,
                    repo,
                    json!({"subagent": {"thread_spawn": {"parent_thread_id": parent_id}}}),
                )],
            );
        };

        // Exact hint day succeeds despite >2048 decoys in unhinted intermediate days.
        fixture.write(
            hint_day,
            &p_exact,
            &[
                codex_meta(&p_exact, repo, json!("cli")),
                codex_user_event(&p_exact, TURN),
            ],
        );
        write_child(&p_exact);
        let rows = fixture.observe(TURN).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            text(&rows[0], "participant"),
            participant_id("codex", CHILD)
        );

        // Timezone margin: parent stored at hint_day - 1 succeeds.
        let p_tz = codex_uuidv7(hint_day, "2");
        fixture.write(
            tz_prev,
            &p_tz,
            &[
                codex_meta(&p_tz, repo, json!("vscode")),
                codex_user_event(&p_tz, TURN),
            ],
        );
        write_child(&p_tz);
        assert_eq!(fixture.observe(TURN).unwrap().len(), 1);

        // Timezone margin: parent stored at hint_day + 1 succeeds.
        std::fs::remove_file(fixture.path(tz_prev, &p_tz)).unwrap();
        fixture.write(
            tz_next,
            &p_tz,
            &[
                codex_meta(&p_tz, repo, json!("cli")),
                codex_user_event(&p_tz, TURN),
            ],
        );
        assert_eq!(fixture.observe(TURN).unwrap().len(), 1);

        // UUIDv7 older than today - 16 has no intersection with lookback window -> 0 dirs opened.
        let p_ancient = codex_uuidv7(ancient_day, "3");
        fixture.write(
            edge_day,
            &p_ancient,
            &[
                codex_meta(&p_ancient, repo, json!("cli")),
                codex_user_event(&p_ancient, TURN),
            ],
        );
        write_child(&p_ancient);
        assert!(fixture.observe(TURN).is_none());
    }

    #[test]
    fn codex_unhinted_older_parent_stops_at_shared_entry_budget() {
        let decoy_day_1 = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let decoy_day_2 = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let parent_day = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();

        let fixture = RolloutFixture::new("older-parent-shared-cap");
        let repo = fixture.store.repo();
        assert_eq!(codex_uuidv7_date(PARENT), None);
        fixture.write(
            parent_day,
            PARENT,
            &[
                codex_meta(PARENT, repo, json!("cli")),
                codex_user_event(PARENT, TURN),
            ],
        );
        fixture.write(fixture.date, CHILD, &[codex_child_meta(CHILD, repo)]);
        assert!(fixture.observe(TURN).is_some());

        // Decoys across earlier days exhaust the shared CODEX_OLDER_PARENT_ENTRY_BUDGET.
        write_codex_decoys(repo, decoy_day_1, 1100);
        write_codex_decoys(repo, decoy_day_2, 1100);
        assert!(fixture.observe(TURN).is_none());
    }

    fn agy_planner(calls: Value) -> Value {
        json!({
            "source": "MODEL", "type": "PLANNER_RESPONSE", "step_index": 5,
            "tool_calls": calls,
        })
    }

    fn agy_result(step: u64, id: &str, workspace: &str) -> Value {
        agy_result_uris(step, id, json!([workspace]))
    }

    fn agy_result_uris(step: u64, id: &str, uris: Value) -> Value {
        let body = json!({"conversationId": id, "workspaceUris": uris});
        json!({
            "source": "MODEL", "type": "GENERIC", "status": "DONE", "step_index": step,
            "content": format!("Created the following subagent:\n{body:#}"),
        })
    }

    fn fixture_children(rows: &[Value]) -> Vec<(String, String, &str)> {
        agy_children(
            rows,
            &mut AgyWorkspaces::new(Path::new("/fixture/repo")),
            None,
        )
        .into_iter()
        .map(|child| (child.id, child.title, child.started))
        .collect()
    }

    fn agy_creation(id: &str, uris: Value) -> [Value; 2] {
        let calls = json!([{"name": "invoke_subagent", "args": {"Role": "researcher"}}]);
        [agy_planner(calls), agy_result_uris(6, id, uris)]
    }

    fn uri(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    /// A real repository with two linked worktrees beside a foreign one with one.
    struct GitFixture {
        root: PathBuf,
    }

    impl GitFixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("oms-agy-worktree-{}-{label}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let fixture = Self { root };
            for (repo, trees) in [("repo", &["wt", "wt2"][..]), ("foreign", &["wt"])] {
                let dir = fixture.root.join(repo);
                std::fs::create_dir(&dir).unwrap();
                fixture.git(&dir, &["init", "-q"]);
                fixture.git(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
                for tree in trees {
                    let name = format!("{repo}-{tree}");
                    let path = fixture.root.join(&name);
                    let path = path.to_str().unwrap();
                    fixture.git(&dir, &["worktree", "add", "-q", "-b", &name, path]);
                }
            }
            fixture
        }

        fn git(&self, dir: &Path, args: &[&str]) {
            let mut command = std::process::Command::new("git");
            for key in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_INDEX_FILE",
                "GIT_COMMON_DIR",
            ] {
                command.env_remove(key);
            }
            let status = command
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("HOME", &self.root)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["-c", "worktree.useRelativePaths=false"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }

        fn accepts(&self, repo: &str, tree: &Path) -> bool {
            let rows = agy_creation(CHILD, json!([uri(tree)]));
            !agy_children(&rows, &mut AgyWorkspaces::new(&self.path(repo)), None).is_empty()
        }

        fn chmod(&self, name: &str, mode: u32) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.path(name);
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }

    impl Drop for GitFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn agy_accepts_private_registered_worktrees_of_the_same_repository() {
        let git = GitFixture::new("accept");
        let (root, tree) = (git.path("repo"), git.path("repo-wt"));
        git.chmod("repo/.git", 0o775);
        git.chmod("repo-wt", 0o775);
        assert!(git.accepts("repo", &root));
        assert!(git.accepts("repo", &tree));
        // A linked checkout resolves the same common directory for its siblings.
        assert!(git.accepts("repo-wt", &git.path("repo-wt2")));
        assert!(!git.accepts("repo", &root.join("sub")));
        assert!(!git.accepts("repo", Path::new(&format!("{}/", tree.display()))));
        assert_eq!(
            observe_checkout(&root, &root),
            Some(("repository".into(), Some("main".into())))
        );
        assert_eq!(
            observe_checkout(&root, &tree),
            Some(("repo-wt".into(), Some("repo-wt".into())))
        );
        assert!(observe_checkout(&root, &git.path("foreign-wt")).is_none());
        git.git(&tree, &["checkout", "--detach", "-q"]);
        assert_eq!(
            observe_checkout(&root, &tree),
            Some(("repo-wt".into(), None))
        );
        std::fs::write(git.path("repo/.git/worktrees/repo-wt/HEAD"), b"abbreviated").unwrap();
        assert!(observe_checkout(&root, &tree).is_none());
    }

    #[test]
    fn agy_limits_group_write_and_rejects_shared_or_linked_metadata() {
        let git = GitFixture::new("modes");
        let tree = git.path("repo-wt");
        let dirs = [
            "repo-wt",
            "repo/.git/worktrees",
            "repo/.git/worktrees/repo-wt",
        ];
        let files = [
            "repo-wt/.git",
            "repo/.git/worktrees/repo-wt/gitdir",
            "repo/.git/worktrees/repo-wt/commondir",
        ];
        let set = |dir_mode, file_mode| {
            dirs.iter().for_each(|dir| git.chmod(dir, dir_mode));
            files.iter().for_each(|file| git.chmod(file, file_mode));
        };
        let objects = || {
            dirs.iter()
                .map(|d| (*d, 0o755))
                .chain(files.map(|f| (f, 0o644)))
        };
        // A repository whose common directory is group-writable admits 0775 trees.
        git.chmod("repo/.git", 0o775);
        set(0o775, 0o664);
        assert!(git.accepts("repo", &tree));
        git.chmod("repo/.git", 0o777);
        assert!(!git.accepts("repo", &tree));
        git.chmod("repo/.git", 0o755);
        set(0o755, 0o644);
        assert!(git.accepts("repo", &tree));
        for (name, mode) in objects() {
            set(0o755, 0o644);
            git.chmod(name, mode | 0o020);
            assert!(!git.accepts("repo", &tree), "group {name}");
        }
        git.chmod("repo/.git", 0o775);
        for (name, mode) in objects() {
            set(0o755, 0o644);
            git.chmod(name, mode | 0o002);
            assert!(!git.accepts("repo", &tree), "other {name}");
        }
        set(0o755, 0o644);
        for file in files {
            std::fs::hard_link(git.path(file), git.path("extra")).unwrap();
            assert!(!git.accepts("repo", &tree), "linked {file}");
            std::fs::remove_file(git.path("extra")).unwrap();
        }
        assert!(git.accepts("repo", &tree));
        // A foreign owner cannot be created unprivileged; check the rule directly.
        let common = fs::stat(git.path("repo/.git")).unwrap();
        let mut stat = fs::stat(&tree).unwrap();
        assert!(agy_private(&stat, &common));
        stat.st_uid = stat.st_uid.wrapping_add(1);
        assert!(!agy_private(&stat, &common));
    }

    #[test]
    fn agy_rejects_foreign_and_symlinked_worktrees() {
        let git = GitFixture::new("foreign");
        let tree = git.path("repo-wt");
        assert!(!git.accepts("repo", &git.path("foreign-wt")));
        std::os::unix::fs::symlink(&tree, git.path("alias")).unwrap();
        assert!(!git.accepts("repo", &git.path("alias")));
        std::os::unix::fs::symlink(&git.root, git.path("hop")).unwrap();
        assert!(!git.accepts("repo", &git.path("hop/repo-wt")));
        std::fs::rename(tree.join(".git"), tree.join("dotgit")).unwrap();
        std::os::unix::fs::symlink("dotgit", tree.join(".git")).unwrap();
        assert!(!git.accepts("repo", &tree));
        std::fs::remove_file(tree.join(".git")).unwrap();
        std::fs::rename(tree.join("dotgit"), tree.join(".git")).unwrap();
        assert!(git.accepts("repo", &tree));
    }

    #[test]
    fn agy_rejects_tampered_oversized_and_pruned_worktree_records() {
        let git = GitFixture::new("tamper");
        let tree = git.path("repo-wt");
        let record = git.path("repo/.git/worktrees/repo-wt/gitdir");
        let original = std::fs::read(&record).unwrap();
        std::fs::write(
            &record,
            format!("{}\n", git.path("repo-wt2/.git").display()),
        )
        .unwrap();
        assert!(!git.accepts("repo", &tree));
        std::fs::write(&record, &original).unwrap();
        assert!(git.accepts("repo", &tree));

        // "/." padding names the same backlink, so only the size bound decides.
        for (size, kept) in [(AGY_META_LIMIT, true), (AGY_META_LIMIT + 1, false)] {
            let mut padded = tree.join(".git").display().to_string();
            while padded.len() + 1 < size {
                padded.push_str(if size - padded.len() > 2 { "/." } else { "/" });
            }
            padded.push('\n');
            assert_eq!(padded.len(), size);
            std::fs::write(&record, padded).unwrap();
            assert_eq!(git.accepts("repo", &tree), kept, "{size}");
        }
        let (dir, _) = agy_dir(&git.path("repo/.git/worktrees/repo-wt")).unwrap();
        // `commondir` holds "../..\n"; the budget must cover it plus one probe byte.
        for (budget, kept) in [(7, true), (6, false)] {
            let mut bytes = budget;
            assert_eq!(agy_meta(&dir, "commondir", &mut bytes).is_some(), kept);
        }
        std::fs::write(&record, &original).unwrap();
        assert!(git.accepts("repo", &tree));
        std::fs::remove_dir_all(&tree).unwrap();
        assert!(!git.accepts("repo", &tree));
        git.git(&git.path("repo"), &["worktree", "prune"]);
        assert!(!git.accepts("repo", &tree));
    }

    #[test]
    fn agy_rejects_duplicate_children_and_bounds_workspace_checks() {
        let duplicated = [
            agy_creation(CHILD, json!([WORKSPACE])),
            agy_creation(CHILD, json!(["file:///fixture/other"])),
            agy_creation(SECOND_CHILD, json!([WORKSPACE])),
        ]
        .concat();
        assert_eq!(
            fixture_children(&duplicated)
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect::<Vec<_>>(),
            [SECOND_CHILD]
        );

        let mut late = (0..AGY_URIS_PER_RECORD)
            .map(|n| json!(format!("file:///fixture/miss{n}")))
            .collect::<Vec<_>>();
        late.push(json!(WORKSPACE));
        assert_eq!(fixture_children(&agy_creation(CHILD, json!(late))).len(), 1);

        // Newer misses exhaust the frame budget before an older genuine worktree,
        // while the newest worktree and the root stay visible.
        let git = GitFixture::new("bounds");
        let root_child = "33333333-3333-3333-3333-333333333333";
        let mut rows = agy_creation(CHILD, json!([uri(&git.path("repo-wt"))])).to_vec();
        rows.extend((0..AGY_URIS_PER_FRAME).flat_map(|n| {
            let id = format!("00000000-0000-0000-0000-{n:012}");
            agy_creation(&id, json!([format!("file:///fixture/miss{n}")]))
        }));
        rows.extend(agy_creation(root_child, json!([uri(&git.path("repo"))])));
        rows.extend(agy_creation(
            SECOND_CHILD,
            json!([uri(&git.path("repo-wt2"))]),
        ));
        let mut workspaces = AgyWorkspaces::new(&git.path("repo"));
        assert_eq!(
            agy_children(&rows, &mut workspaces, None)
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>(),
            [root_child, SECOND_CHILD]
        );
        assert_eq!(workspaces.verdicts.len(), AGY_URIS_PER_FRAME);

        // Past the per-record cap, a genuine worktree is never examined.
        let mut late = (0..AGY_URIS_PER_RECORD)
            .map(|n| json!(format!("file:///fixture/miss{n}")))
            .collect::<Vec<_>>();
        late.push(json!(uri(&git.path("repo-wt"))));
        let mut workspaces = AgyWorkspaces::new(&git.path("repo"));
        assert!(agy_children(&agy_creation(CHILD, json!(late)), &mut workspaces, None).is_empty());
        assert_eq!(workspaces.verdicts.len(), AGY_URIS_PER_RECORD);
    }

    #[test]
    fn agy_projects_each_creation_in_one_batch() {
        let rows = [
            agy_planner(json!([
                {"name": "invoke_subagent", "args": {"Role": "researcher one"}},
                {"name": "invoke_subagent", "args": {"Role": "researcher two"}},
            ])),
            agy_result(6, CHILD, WORKSPACE),
            agy_result(7, SECOND_CHILD, WORKSPACE),
        ];
        assert_eq!(
            fixture_children(&rows)
                .iter()
                .map(|(id, title, _)| (id.as_str(), title.as_str()))
                .collect::<Vec<_>>(),
            [
                (CHILD, "Native subagent"),
                (SECOND_CHILD, "Native subagent")
            ]
        );
    }

    #[test]
    fn agy_honors_mixed_tool_slots_and_batch_boundaries() {
        let calls = json!([
            {"name": "other_tool"},
            {"name": "invoke_subagent", "args": {"Role": "researcher"}},
        ]);
        let earlier_slot = agy_result(6, CHILD, WORKSPACE);
        let matching_slot = agy_result(7, SECOND_CHILD, WORKSPACE);
        let rows = [
            agy_planner(calls.clone()),
            earlier_slot.clone(),
            matching_slot.clone(),
            agy_result(8, CHILD, WORKSPACE),
        ];
        assert_eq!(
            fixture_children(&rows),
            [(SECOND_CHILD.into(), "researcher".into(), "")]
        );

        let foreign_slot = agy_result(7, SECOND_CHILD, "file:///fixture/other");
        let out_of_batch = agy_result(8, SECOND_CHILD, WORKSPACE);
        let rows = [
            agy_planner(calls.clone()),
            earlier_slot.clone(),
            foreign_slot,
            out_of_batch,
        ];
        assert!(fixture_children(&rows).is_empty());

        for (field, value) in [
            ("step_index", json!(4)),
            ("source", json!("SYSTEM")),
            ("type", json!("SYSTEM_MESSAGE")),
            ("status", json!("STARTED")),
        ] {
            let mut boundary = earlier_slot.clone();
            boundary[field] = value;
            let rows = [agy_planner(calls.clone()), boundary, matching_slot.clone()];
            assert!(
                fixture_children(&rows).is_empty(),
                "accepted {field} mismatch"
            );
        }
    }

    fn set_strict_tree(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path).unwrap();
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            for entry in std::fs::read_dir(path).unwrap() {
                set_strict_tree(&entry.unwrap().path());
            }
        } else {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn agy_current_turn_validates_only_last_prompt_line() {
        let older = "0123456789abcdef0123";
        let newer = "fedcba9876543210fedc";
        let screen = format!(
            "> {TASK_PREFIX} Turn {older}. You own this provider's work.\n  output\n  > {TASK_PREFIX} Turn {newer}. You own this provider's work.   \n"
        );
        assert_eq!(agy_current_turn(&screen), Some(newer));
        let bad_last = format!(
            "> {TASK_PREFIX} Turn {older}. You own this provider's work.\n> {TASK_PREFIX} malformed\n"
        );
        assert_eq!(agy_current_turn(&bad_last), None);
        assert_eq!(agy_current_turn("no prompt here"), None);
    }

    #[test]
    fn agy_strict_workspaces_refuses_0775_ancestors_symlinks_hardlinks_and_midflight_swaps() {
        use std::os::unix::fs::PermissionsExt;

        let git = GitFixture::new("strict-ws");
        set_strict_tree(&git.root);

        let repo = git.path("repo");
        let wt = git.path("repo-wt");
        let wt2 = git.path("repo-wt2");
        let foreign = git.path("foreign-wt");

        let mut ws = AgyStrictWorkspaces::new(&repo).unwrap();
        assert!(ws.accepts(&uri(&repo)));
        assert!(ws.accepts(&uri(&wt)));
        assert!(ws.accepts(&uri(&wt2)));
        assert!(!ws.accepts(&uri(&foreign)));
        assert!(ws.confirm());

        // Linked checkout also proves sibling worktrees strictly.
        let mut wt_ws = AgyStrictWorkspaces::new(&wt).unwrap();
        assert!(wt_ws.accepts(&uri(&wt2)));
        assert!(wt_ws.confirm());

        // 0775 .git or 0775 worktree is accepted by legacy AgyWorkspaces, refused by strict.
        git.chmod("repo/.git", 0o775);
        assert!(AgyStrictWorkspaces::new(&repo).is_none());
        git.chmod("repo-wt", 0o775);
        assert!(AgyWorkspaces::new(&repo).accepts(&uri(&wt)));
        git.chmod("repo/.git", 0o700);
        let mut ws = AgyStrictWorkspaces::new(&repo).unwrap();
        assert!(!ws.accepts(&uri(&wt)));
        assert!(ws.confirm());
        git.chmod("repo-wt", 0o700);

        // 0775 on any strict dir/file object fails closed.
        for (rel, mode) in [
            ("repo", 0o775),
            ("repo/.git/worktrees", 0o775),
            ("repo/.git/worktrees/repo-wt", 0o775),
            ("repo-wt/.git", 0o664),
            ("repo/.git/worktrees/repo-wt/gitdir", 0o664),
            ("repo/.git/worktrees/repo-wt/commondir", 0o664),
        ] {
            git.chmod(rel, mode);
            if rel == "repo" {
                assert!(AgyStrictWorkspaces::new(&repo).is_none(), "{rel}");
            } else {
                let mut ws = AgyStrictWorkspaces::new(&repo).unwrap();
                assert!(!ws.accepts(&uri(&wt)), "{rel}");
            }
            let restore = if rel.ends_with(".git") && rel.contains("wt")
                || rel.ends_with("gitdir")
                || rel.ends_with("commondir")
            {
                0o600
            } else {
                0o700
            };
            git.chmod(rel, restore);
        }

        // 0775 parent directory above repo or repo-wt fails strict ancestor check.
        let parent_dir = git.path("parent-box");
        std::fs::create_dir(&parent_dir).unwrap();
        std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let nested_repo = parent_dir.join("repo");
        std::fs::create_dir(&nested_repo).unwrap();
        std::fs::set_permissions(&nested_repo, std::fs::Permissions::from_mode(0o700)).unwrap();
        git.git(&nested_repo, &["init", "-q"]);
        git.git(
            &nested_repo,
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        let nested_wt = parent_dir.join("nested-wt");
        git.git(
            &nested_repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "nested",
                nested_wt.to_str().unwrap(),
            ],
        );
        set_strict_tree(&parent_dir);
        assert!(AgyStrictWorkspaces::new(&nested_repo).is_some());
        std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(AgyStrictWorkspaces::new(&nested_repo).is_none());
        std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        // Separate 0775 parent directory above worktree only fails accepts(wt).
        let wt_parent = git.path("wt-parent");
        std::fs::create_dir(&wt_parent).unwrap();
        std::fs::set_permissions(&wt_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sep_wt = wt_parent.join("sep-wt");
        git.git(
            &nested_repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sep",
                sep_wt.to_str().unwrap(),
            ],
        );
        set_strict_tree(&wt_parent);
        set_strict_tree(&nested_repo);
        let mut ws = AgyStrictWorkspaces::new(&nested_repo).unwrap();
        assert!(ws.accepts(&uri(&sep_wt)));
        std::fs::set_permissions(&wt_parent, std::fs::Permissions::from_mode(0o775)).unwrap();
        let mut ws = AgyStrictWorkspaces::new(&nested_repo).unwrap();
        assert!(!ws.accepts(&uri(&sep_wt)));
        std::fs::set_permissions(&wt_parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        // Hardlink on gitdir / .git fails.
        std::fs::hard_link(wt.join(".git"), git.path("twin-dotgit")).unwrap();
        let mut ws = AgyStrictWorkspaces::new(&repo).unwrap();
        assert!(!ws.accepts(&uri(&wt)));
        std::fs::remove_file(git.path("twin-dotgit")).unwrap();

        // Confirm catches mid-flight chmod or replacement.
        let mut ws = AgyStrictWorkspaces::new(&repo).unwrap();
        assert!(ws.accepts(&uri(&wt)));
        assert!(ws.confirm());
        git.chmod("repo-wt", 0o775);
        assert!(!ws.confirm());
        git.chmod("repo-wt", 0o700);
        assert!(ws.confirm());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agy_row_projection_uses_only_parent_evidence_and_reads_zero_child_files() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        const CHILD_ENV: &str = "OMS_TEST_NATIVE_AGY_PROJ";
        const TEST_NAME: &str = "native_children::tests::agy_row_projection_uses_only_parent_evidence_and_reads_zero_child_files";

        if std::env::var_os(CHILD_ENV).is_some() {
            let repo = PathBuf::from(std::env::var_os("OMS_TEST_REPO").unwrap());
            let lead_pid: u32 = std::env::var("OMS_TEST_PID").unwrap().parse().unwrap();
            let prompt_line = std::env::var("OMS_TEST_PROMPT").unwrap();
            let delivered_id = std::env::var("OMS_TEST_DELIVERED").unwrap();
            let legacy_id = std::env::var("OMS_TEST_LEGACY").unwrap();
            let unknown_id = std::env::var("OMS_TEST_UNKNOWN").unwrap();
            let terminal = format!("> {prompt_line}\n");
            let observe = |provider, pid, repo: &Path, terminal: &str, parent: &mut AgyParent| {
                super::observe(provider, pid, repo, terminal, parent, false)
            };

            reset_agy_legacy_reads();
            let mut cached = AgyParent::default();
            let cold = observe("antigravity", lead_pid, &repo, &terminal, &mut cached);
            assert_eq!(legacy_agy_read_index(), (1, 1, 0, 0));
            assert_eq!(
                agy_legacy_child_reads(),
                0,
                "cold observe read child body files"
            );
            let AgyParent::Legacy(ref file, _) = cached else {
                panic!("expected short parent in legacy cache");
            };
            let legacy_file = file.clone();
            let transcript = PathBuf::from(std::env::var_os("HOME").unwrap())
                .join(".gemini/antigravity-cli/brain")
                .join(file);
            reset_agy_legacy_reads();
            let warm = observe("antigravity", lead_pid, &repo, &terminal, &mut cached);
            assert_eq!(legacy_agy_read_index(), (0, 1, 0, 0));
            assert_eq!(
                agy_legacy_child_reads(),
                0,
                "warm observe read child body files"
            );
            assert_eq!(cold.rows, warm.rows);
            assert_eq!(cold.rows.len(), 3);
            assert!(
                cold.rows
                    .iter()
                    .all(|row| row["native_access_verification"] == "unverified")
            );

            let current = std::fs::read(&transcript).unwrap();
            let birth = oms_panel_core::terminals::process(lead_pid).unwrap().birth;
            assert!(agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &prompt_line
            ));
            assert!(!agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                "01234567890123456789",
                &prompt_line
            ));
            assert!(!agy_submitted_user_turn(
                lead_pid,
                "0",
                &repo,
                TURN,
                &prompt_line
            ));

            let mut quoted: Vec<Value> = current
                .strip_suffix(b"\n")
                .unwrap()
                .split(|&b| b == b'\n')
                .take(2)
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect();
            // This raw model response could match the visible user-row pattern exactly.
            quoted[1] = json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE",
                "status":"DONE","content":format!("> {prompt_line}")});
            let model_quote = quoted
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>();
            std::fs::write(&transcript, &model_quote).unwrap();
            assert!(!agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &prompt_line
            ));

            let header_only = format!("{TASK_PREFIX} Turn {TURN}. You own this provider's work.");
            quoted[1] = json!({"step_index":1,"source":"USER_EXPLICIT","type":"USER_INPUT",
                "status":"DONE","content":format!("<USER_REQUEST>\n{header_only}\n</USER_REQUEST>")});
            let short_request = quoted
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>();
            std::fs::write(&transcript, &short_request).unwrap();
            assert!(!agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &prompt_line
            ));
            assert!(agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &header_only
            ));

            const GEOMETRY_TURN: &str = "014589abcde2345678fa";
            crate::orchestrator::tests::check_ordinary_agy_delivery_geometry(
                GEOMETRY_TURN,
                |body| {
                    assert!(!agy_submitted_user_turn(
                        lead_pid,
                        &birth,
                        &repo,
                        GEOMETRY_TURN,
                        body
                    ));
                    let request = json!({"step_index":1,"source":"USER_EXPLICIT","type":"USER_INPUT",
                        "status":"DONE","content":format!("<USER_REQUEST>\n{body}\n</USER_REQUEST>")});
                    std::fs::write(&transcript, format!("{}\n{request}\n", quoted[0])).unwrap();
                },
                |body| agy_submitted_user_turn(lead_pid, &birth, &repo, GEOMETRY_TURN, body),
            );

            let mut long = vec![b'x'; MAX_TRANSCRIPT];
            long.push(b'\n');
            long.extend_from_slice(&current);
            std::fs::write(&transcript, &long).unwrap();
            assert!(agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &prompt_line
            ));
            reset_agy_legacy_reads();
            let recent = observe("antigravity", lead_pid, &repo, &terminal, &mut cached);
            assert_eq!(
                recent.rows, cold.rows,
                "oversized parent lost current children"
            );
            assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
            assert!(matches!(cached, AgyParent::Bounded(None)));

            let mut boundary = b"\n".to_vec();
            boundary.extend_from_slice(&current);
            let anchor: String = prompt_line.chars().take(200).collect();
            assert_eq!(
                agy_recent_turn(&boundary, true, Some(&anchor)).unwrap().1,
                2
            );
            let wrong = terminal.replace(TURN, "01234567890123456789");
            for screen in [&wrong, "> unmanaged work\n"] {
                assert!(
                    observe("antigravity", lead_pid, &repo, screen, &mut cached)
                        .rows
                        .is_empty()
                );
            }
            let mut incomplete = long.clone();
            incomplete.extend_from_slice(b"{\"type\":\"USER_INPUT\"");
            std::fs::write(&transcript, incomplete).unwrap();
            assert!(!agy_submitted_user_turn(
                lead_pid,
                &birth,
                &repo,
                TURN,
                &prompt_line
            ));
            assert!(
                observe("antigravity", lead_pid, &repo, &terminal, &mut cached)
                    .rows
                    .is_empty()
            );
            let mut newer = long.clone();
            let newer_request = json!({"step_index":9,"source":"USER_EXPLICIT","type":"USER_INPUT",
                "status":"DONE","content":"<USER_REQUEST>\nA newer request\n</USER_REQUEST>"})
            .to_string();
            newer.extend_from_slice(newer_request.as_bytes());
            newer.push(b'\n');
            std::fs::write(&transcript, newer).unwrap();
            assert!(
                observe("antigravity", lead_pid, &repo, &terminal, &mut cached)
                    .rows
                    .is_empty()
            );
            let records: Vec<Value> = current
                .strip_suffix(b"\n")
                .unwrap()
                .split(|&b| b == b'\n')
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect();
            let overwrite = |records: &[Value]| {
                let mut output = vec![b'x'; MAX_TRANSCRIPT];
                for record in records {
                    output.push(b'\n');
                    output.extend_from_slice(record.to_string().as_bytes());
                }
                output.push(b'\n');
                std::fs::write(&transcript, output).unwrap();
            };

            {
                let bounded_bytes = MAX_TRANSCRIPT + 1;
                let bytes_read = || AGY_PARENT_BYTES.with(std::cell::Cell::get);
                let mut board = AgyParent::default();
                let sample = |screen: &str, parent: &mut AgyParent, verified_board| {
                    super::observe(
                        "antigravity",
                        lead_pid,
                        &repo,
                        screen,
                        parent,
                        verified_board,
                    )
                    .rows
                };
                // Mixed workspaces always revalidate the linked tree.
                std::fs::write(&transcript, &long).unwrap();
                reset_agy_legacy_reads();
                for _ in 0..2 {
                    assert_eq!(sample(&terminal, &mut board, true), cold.rows);
                }
                assert_eq!(bytes_read(), 2 * bounded_bytes);
                assert!(matches!(board, AgyParent::Bounded(None)));

                let mut root_rows = records.clone();
                let linked = repo.with_file_name("repo-wt");
                root_rows[5]["content"] =
                    json!(text(&records[5], "content").replace(&uri(&linked), &uri(&repo)));
                overwrite(&root_rows);
                reset_agy_legacy_reads();
                let root_roster = sample(&terminal, &mut board, true);
                assert_eq!(root_roster, cold.rows);
                assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
                assert_eq!(bytes_read(), bounded_bytes);
                assert!(matches!(board, AgyParent::Bounded(Some(_))));
                reset_agy_legacy_reads();
                for _ in 0..20 {
                    assert_eq!(sample(&terminal, &mut board, true), root_roster);
                }
                assert_eq!(legacy_agy_read_index(), (40, 0, 0, 0));
                assert_eq!(bytes_read(), 0);

                let mut delivery = root_rows[8].clone();
                delivery["step_index"] = json!(9);
                let content =
                    text(&delivery, "content").replace("PARENT_DELIVERED_TEXT", "APPENDED_RESULT");
                delivery["content"] = json!(content);
                let mut writer = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&transcript)
                    .unwrap();
                writeln!(writer, "{delivery}").unwrap();
                drop(writer);
                assert!(
                    sample(&terminal, &mut board, true)
                        .iter()
                        .any(|row| row["native_answer"] == "APPENDED_RESULT")
                );
                assert_eq!(bytes_read(), bounded_bytes);

                // Same-size rewrite with restored mtime must still miss on ctime.
                let before = std::fs::metadata(&transcript).unwrap();
                let replacement = std::fs::read_to_string(&transcript)
                    .unwrap()
                    .replace("APPENDED_RESULT", "REWRITTEN_REPLY");
                assert_eq!(replacement.len() as u64, before.len());
                std::fs::write(&transcript, &replacement).unwrap();
                File::open(&transcript)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
                    .unwrap();
                reset_agy_legacy_reads();
                let rewritten = sample(&terminal, &mut board, true);
                assert!(
                    rewritten
                        .iter()
                        .any(|row| row["native_answer"] == "REWRITTEN_REPLY")
                );
                assert_eq!(bytes_read(), bounded_bytes);

                let moved = transcript.with_file_name("replacement.jsonl");
                std::fs::write(&moved, &replacement).unwrap();
                std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::rename(&moved, &transcript).unwrap();
                reset_agy_legacy_reads();
                assert_eq!(sample(&terminal, &mut board, true), rewritten);
                assert_eq!(bytes_read(), bounded_bytes);
                for mode in [0o640, 0o660, 0o600] {
                    std::fs::set_permissions(&transcript, std::fs::Permissions::from_mode(mode))
                        .unwrap();
                    reset_agy_legacy_reads();
                    let shown = sample(&terminal, &mut board, true);
                    if mode == 0o660 {
                        assert!(shown.is_empty() && matches!(board, AgyParent::Bounded(None)));
                        assert_eq!(bytes_read(), 0);
                    } else {
                        assert_eq!(shown, rewritten);
                        assert_eq!(bytes_read(), bounded_bytes);
                    }
                }

                reset_agy_legacy_reads();
                let shorter = terminal.replace(" Extra\n", "\n");
                assert_eq!(sample(&shorter, &mut board, true), rewritten);
                assert_eq!(bytes_read(), bounded_bytes);
                reset_agy_legacy_reads();
                let markerless = "Antigravity terminal ready\n";
                let derived = sample(markerless, &mut board, true);
                assert_eq!(bytes_read(), bounded_bytes);
                assert_eq!(sample(markerless, &mut board, true), derived);
                assert_eq!(bytes_read(), bounded_bytes);
                assert_eq!(sample(markerless, &mut board, false), derived);
                assert_eq!(bytes_read(), 2 * bounded_bytes);
                assert!(matches!(board, AgyParent::Bounded(None)));

                // A successful empty projection has the same metadata fences.
                overwrite(&root_rows[..2]);
                reset_agy_legacy_reads();
                assert!(sample(&terminal, &mut board, true).is_empty());
                assert!(matches!(board, AgyParent::Bounded(Some(_))));
                assert_eq!(bytes_read(), bounded_bytes);
                assert!(sample(&terminal, &mut board, true).is_empty());
                assert_eq!(bytes_read(), bounded_bytes);
                println!(
                    "AGY parent body: {bounded_bytes} bytes cold, 0 bytes across 20 stable frames"
                );
                let short = root_rows[..2]
                    .iter()
                    .map(|row| format!("{row}\n"))
                    .collect::<String>();
                let unterminated = short.strip_suffix('\n').unwrap();
                std::fs::write(&transcript, unterminated).unwrap();
                let mut warm = AgyParent::Legacy(legacy_file.clone(), None);
                let retry_delay = crate::watch::EVENT_BACKED_FALLBACK;
                let sample_at = |screen: &str, parent: &mut AgyParent, now| {
                    agy(lead_pid, &birth, &repo, screen, parent, true, now).unwrap()
                };
                assert!(sample(&terminal, &mut warm, false).is_empty());
                assert!(matches!(warm, AgyParent::Legacy(_, None)));
                reset_agy_legacy_reads();
                assert!(sample(&terminal, &mut warm, true).is_empty());
                assert!(matches!(warm, AgyParent::Legacy(_, Some(_))));
                assert_eq!(legacy_agy_read_index(), (1, 1, 0, 0));
                assert_eq!(bytes_read(), unterminated.len());
                reset_agy_legacy_reads();
                for _ in 0..8 {
                    assert!(sample(&terminal, &mut warm, true).is_empty());
                }
                assert!(
                    sample_at(&terminal, &mut warm, Instant::now() + retry_delay * 2).is_empty()
                );
                assert_eq!(legacy_agy_read_index(), (0, 9, 0, 0));
                assert_eq!(bytes_read(), 0);

                reset_agy_legacy_reads();
                assert!(sample(&shorter, &mut warm, true).is_empty());
                assert_eq!(legacy_agy_read_index(), (1, 1, 0, 0));
                assert_eq!(bytes_read(), unterminated.len());

                // The completed version can arm the original warm slot.
                std::fs::write(&transcript, &short).unwrap();
                reset_agy_legacy_reads();
                assert!(sample(&terminal, &mut warm, true).is_empty());
                assert!(warm.empty_roster_watch().is_some());
                assert_eq!(legacy_agy_read_index(), (2, 2, 0, 0));
                assert_eq!(bytes_read(), short.len());

                // Custody may recover independently of the transcript version.
                let presence = transcript.ancestors().nth(5).unwrap().join("presence");
                std::fs::set_permissions(&presence, std::fs::Permissions::from_mode(0o770))
                    .unwrap();
                let version = AgyLegacyVersion::sample(transcript.clone()).unwrap();
                let mut warm = AgyParent::Legacy(legacy_file, None);
                let started = Instant::now();
                reset_agy_legacy_reads();
                assert!(sample_at(&terminal, &mut warm, started).is_empty());
                assert!(matches!(warm, AgyParent::Legacy(_, Some(_))));
                assert_eq!(legacy_agy_read_index(), (1, 1, 0, 0));
                assert_eq!(bytes_read(), 0);
                reset_agy_legacy_reads();
                for _ in 0..8 {
                    assert!(sample_at(&terminal, &mut warm, started).is_empty());
                }
                assert_eq!(legacy_agy_read_index(), (0, 8, 0, 0));
                assert_eq!(bytes_read(), 0);

                std::fs::set_permissions(&presence, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                assert!(version.matches(&AgyLegacyVersion::sample(transcript.clone()).unwrap()));
                reset_agy_legacy_reads();
                let deadline = started + retry_delay;
                let just_before = deadline - std::time::Duration::from_nanos(1);
                assert!(sample_at(&terminal, &mut warm, just_before).is_empty());
                assert!(warm.empty_roster_watch().is_none());
                assert_eq!(legacy_agy_read_index(), (0, 1, 0, 0));
                assert_eq!(bytes_read(), 0);

                reset_agy_legacy_reads();
                assert!(sample_at(&terminal, &mut warm, deadline).is_empty());
                assert!(warm.empty_roster_watch().is_some());
                assert_eq!(legacy_agy_read_index(), (2, 2, 0, 0));
                assert_eq!(bytes_read(), short.len());
                std::fs::write(&transcript, &long).unwrap();
            }

            let mut tool_omitted = records.clone();
            for index in [2, 4, 6] {
                tool_omitted[index]["truncated_fields"] = json!(["tool_calls"]);
                tool_omitted[index]["tool_calls"][0]["args"] =
                    json!({"Role":"decoy title","Model":"decoy model"});
            }
            let lead = transcript
                .ancestors()
                .nth(3)
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let own_log = transcript
                .ancestors()
                .nth(4)
                .unwrap()
                .join(&delivered_id)
                .join(TRANSCRIPT);
            let first_spawn = |id: &str, workspaces: Value, log: &str| {
                let body = json!({"conversationId":id, "parentConversationId":lead,
                    "workspaceUris":workspaces, "logAbsoluteUri":log});
                format!(
                    "Created At: 2026-11-18T18:00:00Z\nCompleted At: 2026-11-18T18:00:01Z\n{AGY_CREATED}{body:#}\nThe subagent will send you a message when done."
                )
            };
            tool_omitted[3]["content"] = json!(first_spawn(
                &delivered_id,
                json!([uri(&repo)]),
                &uri(&own_log)
            ));
            let working_chrome = format!(
                "{terminal}>\n────────────────────────\n  ● [18:00:55] 4m timer running\n  ● Agent(researcher: Decoy) · Gemini 4 Hydrog...  Generating · 55s\n────────────────────────\nOrchestrator (Carbon)\n"
            );
            assert_eq!(
                crate::provider_activity::observe("antigravity", &working_chrome),
                Some("working")
            );
            overwrite(&tool_omitted);
            reset_agy_legacy_reads();
            let observed = observe("antigravity", lead_pid, &repo, &working_chrome, &mut cached);
            assert_eq!(
                observed.rows.len(),
                3,
                "three complete creation results outlive truncated planner tool calls"
            );
            assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
            assert!(observed.codex.is_empty());
            for id in [&delivered_id, &legacy_id, &unknown_id] {
                let participant = participant_id("antigravity", id);
                let child = observed
                    .rows
                    .iter()
                    .find(|r| r["participant"] == participant)
                    .unwrap();
                assert_eq!(child["state"], "unknown");
                assert_eq!(child["title"], "Native subagent");
                assert_eq!(child["model"], "unknown");
                assert_eq!(child["native_answer"], "");
            }

            // The exact managed Turn is readable by the strict chat locator.
            // Incomplete planner records still cannot confer chat authority.
            let scan_for_chat = |records: &[Value]| {
                let mut bytes = Vec::new();
                for record in records {
                    bytes.extend_from_slice(record.to_string().as_bytes());
                    bytes.push(b'\n');
                }
                let root_uri = uri(&repo);
                let worktree_uri = uri(&repo.with_file_name("repo-wt"));
                crate::agy_child_records::scan_lead_turn(
                    &bytes,
                    false,
                    lead,
                    Some(TURN),
                    |workspace| workspace == root_uri || workspace == worktree_uri,
                )
            };
            assert!(scan_for_chat(&tool_omitted[..2]).is_some());
            assert!(scan_for_chat(&tool_omitted).is_none());

            let observe_candidate = |candidate: &[Value], cache: &mut AgyParent| {
                overwrite(candidate);
                observe("antigravity", lead_pid, &repo, &terminal, cache).rows
            };
            let upper_parent = lead.to_ascii_uppercase();
            let upper_log = transcript
                .ancestors()
                .nth(4)
                .unwrap()
                .join(&upper_parent)
                .join(TRANSCRIPT);
            for bad in [
                first_spawn(
                    &delivered_id,
                    json!([uri(&repo), "file:///fixture/foreign"]),
                    &uri(&own_log),
                ),
                first_spawn(
                    &delivered_id,
                    json!([uri(&repo)]),
                    "file:///fixture/foreign/transcript.jsonl",
                ),
                first_spawn(&upper_parent, json!([uri(&repo)]), &uri(&upper_log)),
                format!("unproved output\n{}", text(&tool_omitted[3], "content")),
            ] {
                let mut candidate = tool_omitted.clone();
                candidate[3]["content"] = json!(bad);
                assert_eq!(observe_candidate(&candidate, &mut cached).len(), 2);
            }
            let mut candidate = tool_omitted.clone();
            candidate[2]["tool_calls"][0]["name"] = json!("read_files");
            assert_eq!(observe_candidate(&candidate, &mut cached).len(), 2);
            candidate[2]["tool_calls"] = tool_omitted[2]["tool_calls"].clone();
            candidate[2]["tool_calls"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"invoke_subagent"}));
            // Slot two has no result. Slot one is not recovered as a complete prefix.
            assert_eq!(observe_candidate(&candidate, &mut cached).len(), 2);
            candidate[2]["tool_calls"] = json!([]);
            assert_eq!(observe_candidate(&candidate, &mut cached).len(), 2);

            let mut batch = tool_omitted.clone();
            let second_planner = batch.remove(4);
            batch[2]["tool_calls"]
                .as_array_mut()
                .unwrap()
                .push(second_planner["tool_calls"][0].clone());
            for (step, record) in batch.iter_mut().enumerate() {
                record["step_index"] = json!(step);
            }
            assert_eq!(observe_candidate(&batch, &mut cached).len(), 3);
            let legacy_log = transcript
                .ancestors()
                .nth(4)
                .unwrap()
                .join(&legacy_id)
                .join(TRANSCRIPT);
            batch[4]["content"] = json!(first_spawn(
                &legacy_id,
                json!([uri(&repo), "file:///fixture/foreign"]),
                &uri(&legacy_log)
            ));
            let withheld = observe_candidate(&batch, &mut cached);
            assert_eq!(withheld.len(), 1);
            assert_eq!(
                withheld[0]["participant"],
                participant_id("antigravity", &unknown_id)
            );

            let fourth_id = "44444444-2222-3333-4444-888888888888";
            let stamps = "Created At: 2026-11-18T18:00:00Z\nCompleted At: 2026-11-18T18:00:01Z\n";
            let footer = "The subagent will send you a message when it has completed its task or requires guidance. There is no need to poll for its response.";
            let envelope = |id: &str, workspace: &Path| {
                let log = transcript
                    .ancestors()
                    .nth(4)
                    .unwrap()
                    .join(id)
                    .join(TRANSCRIPT);
                let body = json!({"conversationId":id, "parentConversationId":lead,
                    "workspaceUris":[uri(workspace)], "logAbsoluteUri":uri(&log)});
                format!("{stamps}{stamps}{AGY_CREATED}{body:#}\n{footer}")
            };
            let mut three_and_one: Vec<_> = [0, 1, 2, 3, 5, 7, 6, 7, 8]
                .map(|index| tool_omitted[index].clone())
                .into();
            three_and_one[2]["tool_calls"] =
                json!([2, 4, 6].map(|index| tool_omitted[index]["tool_calls"][0].clone()));
            three_and_one[6]["tool_calls"][0]["id"] = json!("t4");
            let worktree = repo.with_file_name("repo-wt");
            let ids = [
                delivered_id.as_str(),
                legacy_id.as_str(),
                unknown_id.as_str(),
                fourth_id,
            ];
            for ((index, id), workspace) in [3, 4, 5, 7]
                .into_iter()
                .zip(ids)
                .zip([&repo, &worktree, &repo, &worktree])
            {
                three_and_one[index]["content"] = json!(envelope(id, workspace));
            }
            for (step, row) in three_and_one.iter_mut().enumerate() {
                row["step_index"] = json!(step);
            }
            let owner = terminals::process(lead_pid).unwrap();
            let custody = crate::agy_lead_custody::agy_lead_custody(lead_pid, &owner.birth, &repo)
                .expect("exact stand-in lead custody");
            let candidates = agy_children(
                &three_and_one,
                &mut AgyWorkspaces::new(&repo),
                Some(AgyDisplayScope {
                    first_spawn: 2,
                    custody: &custody,
                    derived: false,
                }),
            );
            assert_eq!(
                candidates
                    .iter()
                    .map(|child| child.id.as_str())
                    .collect::<Vec<_>>(),
                ids,
                "all four creation records must be admitted"
            );
            reset_agy_legacy_reads();
            let visible = observe_candidate(&three_and_one, &mut cached);
            assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
            assert_eq!(
                visible
                    .iter()
                    .map(|row| text(row, "participant"))
                    .collect::<Vec<_>>(),
                [fourth_id, &unknown_id, &legacy_id, &delivered_id]
                    .map(|id| participant_id("antigravity", id))
            );
            assert!(
                visible
                    .iter()
                    .all(|row| row["state"] == "unknown" && row["native_answer"] == "")
            );
            assert!(scan_for_chat(&three_and_one).is_none());

            let markerless = working_chrome.strip_prefix(&terminal).unwrap();
            assert!(agy_current_turn(markerless).is_none());
            let mut fresh = AgyParent::default();
            for screen in [
                markerless,
                "Tool results are visible, including the phrase Task from Overall Main.",
                "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)\n────\n",
            ] {
                reset_agy_legacy_reads();
                let markerless_rows = observe("antigravity", lead_pid, &repo, screen, &mut fresh);
                assert_eq!(
                    markerless_rows.rows, visible,
                    "exact current parent proves markerless display rows"
                );
                assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
                assert!(agy_current_turn(screen).is_none());
                assert!(markerless_rows.codex.is_empty());
                assert!(matches!(fresh, AgyParent::Unseen));
            }
            let metadata = "\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-22T12:34:56+09:00.\n</ADDITIONAL_METADATA>";
            let with_metadata = |suffix: &str| {
                let mut records = three_and_one.clone();
                records[1]["content"] = json!(format!(
                    "<USER_REQUEST>\n{prompt_line}\n</USER_REQUEST>{suffix}"
                ));
                overwrite(&records);
            };
            with_metadata(metadata);
            reset_agy_legacy_reads();
            assert_eq!(
                observe("antigravity", lead_pid, &repo, markerless, &mut fresh).rows,
                visible,
                "native time metadata must preserve the markerless projection"
            );
            assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
            assert!(agy_current_turn(markerless).is_none());
            for invalid in [
                metadata.replace("2026-11-22", "2026-13-22"),
                metadata.replace("T12", " 12"),
                metadata.replace("56+", "56.12345678901234567890+"),
                metadata.replace("The current local time is: ", "Another metadata field: "),
                format!("{metadata}\nTrailing text"),
                format!("{metadata}\n</ADDITIONAL_METADATA>"),
                format!("\n<ADDITIONAL_METADATA>{metadata}"),
            ] {
                with_metadata(&invalid);
                assert!(
                    observe("antigravity", lead_pid, &repo, markerless, &mut fresh)
                        .rows
                        .is_empty()
                );
            }
            overwrite(&three_and_one);
            let mut legacy = AgyParent::Legacy(PathBuf::from("unrelated/unchanged.jsonl"), None);
            reset_agy_legacy_reads();
            assert_eq!(
                observe("antigravity", lead_pid, &repo, markerless, &mut legacy).rows,
                visible
            );
            assert_eq!(legacy_agy_read_index(), (2, 1, 0, 0));
            assert!(
                matches!(legacy, AgyParent::Legacy(ref path, None) if path == Path::new("unrelated/unchanged.jsonl"))
            );
            let ambiguous_capture = format!("{terminal}{terminal}{markerless}");
            let malformed = format!("{terminal}> {TASK_PREFIX} Turn clipped\n{markerless}");
            let squeezed = format!(">{prompt_line}\n{markerless}");
            let padded = format!(">  {prompt_line}\n{markerless}");
            let prior_screen = format!("{wrong}{terminal}{markerless}");
            let stale_anchor = format!("{terminal}> unmanaged work\n{markerless}");
            assert_eq!(
                observe("antigravity", lead_pid, &repo, &prior_screen, &mut cached).rows,
                visible
            );
            assert!(
                observe(
                    "antigravity",
                    lead_pid,
                    &repo,
                    &format!("{wrong}{markerless}"),
                    &mut cached
                )
                .rows
                .is_empty()
            );
            for screen in [
                "",
                "> unmanaged work\n",
                "> unmanaged work\n>\n",
                "> unmanaged work\n> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)\n",
                "\x1b[0munknown\n",
                &ambiguous_capture,
                &malformed,
                &squeezed,
                &padded,
                &stale_anchor,
            ] {
                reset_agy_legacy_reads();
                assert!(
                    observe("antigravity", lead_pid, &repo, screen, &mut fresh)
                        .rows
                        .is_empty()
                );
                assert_eq!(legacy_agy_read_index(), (0, 0, 0, 0));
            }
            for rows in [
                json!({"step_index":9,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE",
                    "content":format!("<USER_REQUEST>\n{prompt_line}\n</USER_REQUEST>")}),
                json!({"step_index":9,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE",
                    "content":format!("<USER_REQUEST>\n{}\n</USER_REQUEST>", prompt_line.replace(TURN,"01234567890123456789"))}),
                json!({"step_index":9,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE",
                    "content":"<USER_REQUEST>\nUnmanaged work\n</USER_REQUEST>"}),
                json!({"step_index":9,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE",
                    "content":"<USER_REQUEST>\nUnclosed wrapper"}),
                json!({"step_index":9,"type":"PLANNER_RESPONSE","source":"USER_EXPLICIT","status":"DONE"}),
            ] {
                let mut bad = three_and_one.clone();
                bad.push(rows);
                overwrite(&bad);
                assert!(
                    observe("antigravity", lead_pid, &repo, markerless, &mut fresh)
                        .rows
                        .is_empty()
                );
            }
            let mut gap = three_and_one.clone();
            gap[0]["step_index"] = json!(10);
            overwrite(&gap);
            assert!(
                observe("antigravity", lead_pid, &repo, markerless, &mut fresh)
                    .rows
                    .is_empty()
            );
            let mut summary = three_and_one.clone();
            summary[1]["content"] = json!(format!(
                "<USER_REQUEST>\n{prompt_line}\n<CONTEXT_SUMMARY>\n</USER_REQUEST>"
            ));
            overwrite(&summary);
            assert!(
                observe("antigravity", lead_pid, &repo, markerless, &mut fresh)
                    .rows
                    .is_empty()
            );
            overwrite(&three_and_one);
            let mut fresh = AgyParent::default();
            reset_agy_legacy_reads();
            assert!(
                agy(
                    lead_pid,
                    "bad-birth",
                    &repo,
                    markerless,
                    &mut fresh,
                    false,
                    Instant::now()
                )
                .is_none()
            );
            assert!(
                agy(
                    lead_pid,
                    &owner.birth,
                    &worktree,
                    markerless,
                    &mut fresh,
                    false,
                    Instant::now()
                )
                .is_none()
            );
            assert!(
                observe("antigravity", 0, &repo, markerless, &mut fresh)
                    .rows
                    .is_empty()
            );
            assert_eq!(legacy_agy_read_index(), (2, 0, 0, 0));

            overwrite(&records);
            let intact = observe("antigravity", lead_pid, &repo, markerless, &mut fresh);
            assert_eq!(intact.rows.len(), 3);
            assert!(
                intact
                    .rows
                    .iter()
                    .all(|row| row["state"] == "unknown" && row["native_answer"] == "")
            );
            let mut foreign = records.clone();
            foreign[3]["content"] = json!(first_spawn(
                &delivered_id,
                json!([uri(&repo), "file:///fixture/foreign"]),
                &uri(&own_log)
            ));
            overwrite(&foreign);
            assert_eq!(
                observe("antigravity", lead_pid, &repo, markerless, &mut fresh)
                    .rows
                    .len(),
                2
            );

            let valid = envelope(&delivered_id, &repo);
            let body = valid.strip_prefix(&format!("{stamps}{stamps}")).unwrap();
            let changed_stamp = stamps.replace("18:00:01Z", "18:00:02Z");
            let invalid_stamp = stamps.replace("18:00:01Z", "not-a-timestamp");
            for malformed in [
                format!("{stamps}{changed_stamp}{body}"),
                format!("{invalid_stamp}{invalid_stamp}{body}"),
                format!("{stamps}{stamps}{stamps}{body}"),
                format!("{stamps}Completed At: 2026-11-18T18:00:01Z\n{body}"),
                format!("{valid}\nUnexpected reply"),
                valid.trim_end_matches(footer).to_owned(),
                valid.replace("There is no need to poll", "Poll"),
            ] {
                assert!(agy_recovered_result(&malformed, &custody).is_none());
            }
            assert_eq!(agy_legacy_child_reads(), 0);

            let mut ambiguous = records.clone();
            ambiguous[1]["content"] = json!(format!(
                "<USER_REQUEST>\n{prompt_line}\n{prompt_line}\n</USER_REQUEST>"
            ));
            overwrite(&ambiguous);
            assert!(
                observe("antigravity", lead_pid, &repo, &terminal, &mut cached)
                    .rows
                    .is_empty()
            );
            for (index, expected_count, expected_delivered_state) in
                [(3, 2, None), (8, 3, Some("unknown"))]
            {
                let mut truncated = records.clone();
                truncated[index]["truncated_fields"] = json!(["content"]);
                overwrite(&truncated);
                let rows = observe("antigravity", lead_pid, &repo, &terminal, &mut cached).rows;
                let id = participant_id("antigravity", &delivered_id);
                assert_eq!(rows.len(), expected_count);
                assert_eq!(
                    rows.iter()
                        .find(|row| row["participant"] == id)
                        .and_then(|row| row["state"].as_str()),
                    expected_delivered_state
                );
            }
            let mut shortened = current.clone();
            shortened.extend_from_slice(newer_request.as_bytes());
            shortened.push(b'\n');
            std::fs::write(&transcript, shortened).unwrap();
            assert!(
                observe("antigravity", lead_pid, &repo, &terminal, &mut cached)
                    .rows
                    .is_empty(),
                "shortening must not restore legacy any-history matching"
            );
            std::fs::write(&transcript, &long).unwrap();

            // The legacy scan could find this unrelated short file with the same prompt.
            // A pane that has required a bound tail must refuse if its proof disappears.
            let replica = transcript
                .ancestors()
                .nth(4)
                .unwrap()
                .join("55555555-2222-3333-4444-777777777777")
                .join(TRANSCRIPT);
            std::fs::create_dir_all(replica.parent().unwrap()).unwrap();
            std::fs::write(&replica, &current).unwrap();
            assert_eq!(
                agy(
                    lead_pid,
                    "bad-birth",
                    &repo,
                    &terminal,
                    &mut AgyParent::default(),
                    false,
                    Instant::now(),
                )
                .unwrap()
                .len(),
                3
            );
            assert!(
                agy(
                    lead_pid,
                    "bad-birth",
                    &repo,
                    &terminal,
                    &mut cached,
                    false,
                    Instant::now()
                )
                .is_none()
            );
            assert_eq!(agy_legacy_child_reads(), 0);

            let by_id = |cid: &str| {
                let p = participant_id("antigravity", cid);
                cold.rows
                    .iter()
                    .find(|r| r["participant"] == p)
                    .unwrap_or_else(|| panic!("missing row for {cid}: {:?}", cold.rows))
            };

            // 1. Delivered child uses exact post-spawn delivery, keeps "unknown" for Model="inherit" despite planner.model, ignoring child file.
            let del = by_id(&delivered_id);
            assert_eq!(del["state"], "done");
            assert_eq!(del["model"], "unknown");
            assert_eq!(del["native_answer"], "PARENT_DELIVERED_TEXT");
            assert_eq!(del["updated_at"], "2026-11-18T18:02:00Z");

            // 2. Negative 0775-wt legacy child with explicit Model="flash", disk reply and no parent delivery stays unknown + empty.
            let leg = by_id(&legacy_id);
            assert_eq!(leg["state"], "unknown");
            assert_eq!(leg["model"], "flash");
            assert_eq!(leg["native_answer"], "");
            assert!(!leg.to_string().contains("UNPROVED_LEGACY_CHILD_REPLY"));

            // 3. Undelivered child with omitted Model stays "unknown" even when PLANNER_RESPONSE carries model.
            let unk = by_id(&unknown_id);
            assert_eq!(unk["state"], "unknown");
            assert_eq!(unk["model"], "unknown");
            assert_eq!(unk["native_answer"], "");

            // Direct model-selection checks: "inherit" and "default" stay "unknown"; explicit child model wins.
            assert_eq!(
                agy_child_model(&json!({}), &json!({"args": {"Model": "inherit"}})),
                "unknown"
            );
            assert_eq!(
                agy_child_model(&json!({}), &json!({"args": {"Model": "default"}})),
                "unknown"
            );
            assert_eq!(
                agy_child_model(&json!({}), &json!({"args": {"model": "Default"}})),
                "unknown"
            );
            assert_eq!(
                agy_child_model(&json!({}), &json!({"args": {"Model": "flash"}})),
                "flash"
            );

            // 4. agy_delivery rejects bad blocks, extra tokens, newlines, pre-spawn deliveries, and prefixes.
            let preamble = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
            let prefix_spoof = format!(
                "{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:00Z sender={delivered_id}x priority=HIGH content=BAD\n</SYSTEM_MESSAGE>"
            );
            assert_eq!(agy_delivery(&prefix_spoof, &delivered_id), None);
            let extra_tok = format!(
                "{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:00Z sender={delivered_id} priority=HIGH role=extra content=BAD\n</SYSTEM_MESSAGE>"
            );
            assert_eq!(agy_delivery(&extra_tok, &delivered_id), None);
            let no_ts = format!(
                "<SYSTEM_MESSAGE>\n[Message] sender={delivered_id} content=BAD\n</SYSTEM_MESSAGE>"
            );
            assert_eq!(agy_delivery(&no_ts, &delivered_id), None);
            let embedded_close = format!(
                "<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:00Z sender={delivered_id} content=A</SYSTEM_MESSAGE>B\n</SYSTEM_MESSAGE>"
            );
            assert_eq!(agy_delivery(&embedded_close, &delivered_id), None);
            return;
        }

        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-agy-native-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mkdir_p = |p: &Path| {
            std::fs::create_dir_all(p).unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        let repo = base.join("repo");
        let wt = base.join("repo-wt");
        let home = base.join("home");
        let app_root = home.join(".gemini/antigravity-cli");
        let presence = app_root.join("presence");
        let brain = app_root.join("brain");
        for d in [
            &base,
            &repo,
            &home,
            &home.join(".gemini"),
            &app_root,
            &presence,
            &brain,
        ] {
            mkdir_p(d);
        }
        let git_bin = |dir: &Path, args: &[&str]| {
            let mut c = Command::new("git");
            for k in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_INDEX_FILE",
                "GIT_COMMON_DIR",
            ] {
                c.env_remove(k);
            }
            assert!(
                c.current_dir(dir)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("HOME", &home)
                    .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                    .args([
                        "-c",
                        "init.defaultBranch=main",
                        "-c",
                        "commit.gpgsign=false"
                    ])
                    .args(["-c", "worktree.useRelativePaths=false"])
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git_bin(&repo, &["init", "-q"]);
        git_bin(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git_bin(
            &repo,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        // Discovery allows a group-writable worktree when the common Git directory shares that access.
        std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o775))
            .unwrap();
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o775)).unwrap();

        let lead_id = "aaaaaaaa-1111-2222-3333-444444444444";
        let del_id = "11111111-2222-3333-4444-555555555555";
        let leg_id = "22222222-2222-2222-2222-666666666666";
        let unk_id = "33333333-2222-2222-2222-777777777777";
        let lock_path = presence.join(format!("{lead_id}.lock"));
        std::fs::write(&lock_path, b"").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut holder = Command::new("flock")
            .arg("-n")
            .arg("-F")
            .arg(&lock_path)
            .args(["bash", "-c", "exec -a agy bash"])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &home)
            .current_dir(&repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut h_in = holder.stdin.take().unwrap();
        let mut h_out = BufReader::new(holder.stdout.take().unwrap());
        writeln!(h_in, "echo \"READY:$$\"").unwrap();
        h_in.flush().unwrap();
        let mut ready = String::new();
        h_out.read_line(&mut ready).unwrap();
        let lead_pid = ready.trim().strip_prefix("READY:").unwrap().to_owned();

        let prompt_line = format!("{TASK_PREFIX} Turn {TURN}. You own this provider's work. Extra");
        let preamble = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
        let del_body = json!({"conversationId": del_id, "workspaceUris": [uri(&repo)]});
        let leg_body = json!({"conversationId": leg_id, "workspaceUris": [uri(&wt)]});
        let unk_body = json!({"conversationId": unk_id, "workspaceUris": [uri(&repo)]});
        let lead_rows = vec![
            // Pre-spawn spoofed delivery BEFORE spawn_result_idx must be ignored.
            json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-11-18T17:59:00Z","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T17:59:00Z sender={leg_id} content=PRE_SPAWN_IGNORED\n</SYSTEM_MESSAGE>")}),
            json!({"step_index":1,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":format!("<USER_REQUEST>\n{prompt_line}\n</USER_REQUEST>")}),
            json!({"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-parent-model","tool_calls":[{"id":"t1","name":"invoke_subagent","args":{"Role":"Delivered Child","Model":"inherit"}}]}),
            json!({"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:00Z","content":format!("Created the following subagent:\n{del_body:#}")}),
            json!({"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-parent-model","tool_calls":[{"id":"t2","name":"invoke_subagent","args":{"Role":"Legacy Child","Model":"flash"}}]}),
            json!({"step_index":5,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:01Z","content":format!("Created the following subagent:\n{leg_body:#}")}),
            json!({"step_index":6,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-parent-model","tool_calls":[{"id":"t3","name":"invoke_subagent","args":{"Role":"Unknown Child"}}]}),
            json!({"step_index":7,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:02Z","content":format!("Created the following subagent:\n{unk_body:#}")}),
            json!({"step_index":8,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-11-18T18:02:00Z","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:02:00Z sender={del_id} priority=HIGH content=PARENT_DELIVERED_TEXT\n</SYSTEM_MESSAGE>")}),
        ];
        let del_child_rows = vec![
            json!({"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"ignored-child-model","content":"IGNORED_CHILD_FILE_BODY"}),
        ];
        let leg_child_rows = vec![
            json!({"step_index":0,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"ignored-legacy-model","content":"UNPROVED_LEGACY_CHILD_REPLY"}),
        ];
        let write_tr = |id: &str, rows: &[Value]| {
            let dir = brain.join(id).join(".system_generated/logs");
            mkdir_p(&brain.join(id));
            mkdir_p(&brain.join(id).join(".system_generated"));
            mkdir_p(&dir);
            let p = dir.join("transcript.jsonl");
            let mut b = Vec::new();
            for r in rows {
                b.extend_from_slice(r.to_string().as_bytes());
                b.push(b'\n');
            }
            std::fs::write(&p, b).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        write_tr(lead_id, &lead_rows);
        write_tr(del_id, &del_child_rows);
        write_tr(leg_id, &leg_child_rows);

        let exe = std::env::current_exe().unwrap();
        let out = Command::new(&exe)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &home)
            .env(CHILD_ENV, "1")
            .env("OMS_TEST_REPO", &repo)
            .env("OMS_TEST_PID", &lead_pid)
            .env("OMS_TEST_PROMPT", &prompt_line)
            .env("OMS_TEST_DELIVERED", del_id)
            .env("OMS_TEST_LEGACY", leg_id)
            .env("OMS_TEST_UNKNOWN", unk_id)
            .output()
            .unwrap();
        let _ = holder.kill();
        let _ = holder.wait();
        let _ = std::fs::remove_dir_all(&base);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "stdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
