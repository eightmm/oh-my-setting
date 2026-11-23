//! Chat of the Claude Code subagents a lead pane runs for one managed Turn.
//!
//! Proof chain: the lead's kernel start time equals `procStart` in Claude
//! Code's `sessions/<pid>.json` (an undocumented internal record) whose `cwd`
//! is the repository and whose `sessionId` names the lead transcript. The last
//! managed Turn in that transcript must be the requested one, every lead entry
//! from it on must be positively non-sidechain, and a child counts only when
//! an `Agent` call after the Turn entry spawned it directly, no `SendMessage`
//! of this Turn addressed it, and its `meta.json` names that call at spawn
//! depth 1. Chat comes only from the child's own replies after its spawn
//! prompt, never from text the parent saw (tool results, notifications, its
//! own narration). Every directory and file of the walk must be owned by the
//! caller and writable by no one else, and a second walk by name must reach the
//! same inodes after the reads.

/// Holds only facts that stay fixed while the transcripts grow, so a chat read
/// can demand the identical locator. No `Debug`: ids are transcript facts.
/// `boundary` is the sha256 of the spawn prompt, never the prompt itself.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClaudeChildLocator {
    pub(crate) pid: u32,
    pub(crate) birth: String,
    pub(crate) session: String,
    pub(crate) turn: String,
    pub(crate) agent: String,
    pub(crate) tool_use: String,
    pub(crate) boundary: String,
    pub(crate) lead: (rustix::fs::Dev, u64),
    pub(crate) child: (rustix::fs::Dev, u64),
}

/// A proven child: the locator binds chat reads, the title only paints a row.
pub(crate) struct ClaudeChild {
    pub(crate) locator: ClaudeChildLocator,
    pub(crate) title: String,
    /// "done" only on the lead's own exact completion notice, else "unknown".
    pub(crate) state: &'static str,
    /// `None` is withheld: not read for this sample, or the read proved nothing.
    pub(crate) usage: Option<ClaudeChildUsage>,
}

/// What the Board may show of one child: its newest response and the lead's
/// own clocks. A `None` field is unknown; nothing here measures a duration.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClaudeChildUsage {
    pub(crate) response: Option<ClaudeResponse>,
    /// Clock of the lead's `Agent` call row.
    pub(crate) spawned_at: Option<String>,
    /// Clock of the lead's exact completion notice.
    pub(crate) delivered_at: Option<String>,
}

/// One response of the child itself: its `message.id`, the clock of its last
/// row and its own counters in `USAGE_KEYS` order, never cumulative.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClaudeResponse {
    pub(crate) id: String,
    pub(crate) at: String,
    pub(crate) usage: [u64; 4],
}

pub(crate) const USAGE_KEYS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
];

#[cfg(not(target_os = "linux"))]
pub(crate) fn claude_current_children(
    _pid: u32,
    _repo: &std::path::Path,
) -> Option<(String, Vec<ClaudeChild>)> {
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn claude_child_chat(
    _repo: &std::path::Path,
    _turn: &str,
    _expected: &ClaudeChildLocator,
) -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn claude_board_children(
    _pid: u32,
    _repo: &std::path::Path,
) -> Option<(String, Vec<ClaudeChild>)> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{claude_board_children, claude_child_chat, claude_current_children};

#[cfg(target_os = "linux")]
mod linux {
    use super::{ClaudeChild, ClaudeChildLocator, ClaudeChildUsage, ClaudeResponse, USAGE_KEYS};
    use crate::claude_lead_usage::linux::{
        DIR, FILE, config_root, environ, open_dir, start_time, unchanged,
    };
    use crate::native_children::{CLAUDE_ROSTER_LIMIT, TASK_PREFIX, chat_tail, claude_completion};
    use crate::{rows, text};
    use oms_panel_core::{Value, digest, terminals};
    use rustix::fs::{self, FileType, Mode, Stat};
    use rustix::process::geteuid;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    const RECORD_LIMIT: u64 = 16 * 1024;
    const TRANSCRIPT_LIMIT: u64 = 1 << 30;
    pub(super) const LEAD_WINDOW: u64 = 4 * 1024 * 1024;
    pub(super) const CHILD_WINDOW: u64 = 1024 * 1024;
    pub(super) const LINE_LIMIT: usize = 256 * 1024;
    /// Child tails a Board sample may read for usage, newest spawns first: at
    /// most `USAGE_TAILS * (CHILD_WINDOW + 1)` bytes beyond the lead's window.
    pub(super) const USAGE_TAILS: usize = 2;

    #[cfg(test)]
    // Counts child metadata reads, so tests can prove the roster bound.
    thread_local!(pub(super) static META_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

    /// `Err` names the first gate that refused, for diagnosis only.
    pub(super) type Stage<T> = Result<T, &'static str>;

    fn gate(ok: bool, stage: &'static str) -> Stage<()> {
        if ok { Ok(()) } else { Err(stage) }
    }

    /// The lead's last managed Turn and its direct children. OMS does not keep
    /// the Turn it sent and the pane collapses pasted prompts, so the proven
    /// transcript is the only record of which Turn is current.
    pub(crate) fn claude_current_children(
        pid: u32,
        repo: &Path,
    ) -> Option<(String, Vec<ClaudeChild>)> {
        proven(pid, |birth| {
            let config = lead_config(pid)?;
            current(&Lead::new(pid, birth, &config), repo, 0, || {})
        })
        .ok()
    }

    /// The same children, with usage read for at most `USAGE_TAILS` of them.
    pub(crate) fn claude_board_children(
        pid: u32,
        repo: &Path,
    ) -> Option<(String, Vec<ClaudeChild>)> {
        proven(pid, |birth| {
            let config = lead_config(pid)?;
            current(&Lead::new(pid, birth, &config), repo, USAGE_TAILS, || {})
        })
        .ok()
    }

    /// A failed usage read withholds that child's usage, never its row.
    /// `between` runs after the usage reads and before their recheck (a test seam).
    pub(super) fn current(
        lead: &Lead,
        repo: &Path,
        tails: usize,
        between: impl FnOnce(),
    ) -> Stage<(String, Vec<ClaudeChild>)> {
        let mut scan = scan(lead, repo, None)?;
        scan.confirm()?;
        let skip = scan.children.len().saturating_sub(tails);
        let read: Vec<_> = scan
            .children
            .iter()
            .enumerate()
            .map(|(at, child)| (at >= skip).then(|| usage(&scan, &child.locator).ok())?)
            .collect();
        between();
        // The usage reads widened the window, so the scan must still hold after them.
        if read.iter().any(Option::is_some) && scan.confirm().is_ok() {
            for (child, usage) in scan.children.iter_mut().zip(read) {
                child.usage = usage;
            }
        }
        Ok((scan.turn, scan.children))
    }

    pub(crate) fn claude_child_chat(
        repo: &Path,
        turn: &str,
        expected: &ClaudeChildLocator,
    ) -> Option<String> {
        proven(expected.pid, |birth| {
            let config = lead_config(expected.pid)?;
            chat(
                &Lead::new(expected.pid, birth, &config),
                repo,
                turn,
                expected,
                || {},
            )
        })
        .ok()
    }

    /// The config root comes from the lead's initial environment, never the panel's.
    pub(super) fn lead_config(pid: u32) -> Stage<PathBuf> {
        config_root(&environ(pid).ok_or("environ")?).ok_or("config-root")
    }

    pub(super) struct Lead<'a> {
        pid: u32,
        birth: &'a str,
        config: &'a Path,
    }

    impl<'a> Lead<'a> {
        pub(super) fn new(pid: u32, birth: &'a str, config: &'a Path) -> Self {
            Self { pid, birth, config }
        }
    }

    /// A pid that exited or was reused while the files were read proves nothing.
    pub(super) fn proven<T>(pid: u32, read: impl FnOnce(&str) -> Stage<T>) -> Stage<T> {
        let owner = terminals::process(pid).map_err(|_| "process")?;
        gate(owner.uid == geteuid().as_raw(), "process-owner")?;
        let found = read(&owner.birth)?;
        gate(
            terminals::process(pid).is_ok_and(|now| now == owner),
            "process-changed",
        )?;
        Ok(found)
    }

    /// Every name the reads resolved, with the inode it reached, for the re-walk.
    #[derive(Default)]
    struct Trail(Vec<(Vec<String>, bool, (fs::Dev, u64))>);

    /// An opened directory of the walk. Owned by the caller, no group or other write.
    struct Dir {
        fd: OwnedFd,
        names: Vec<String>,
        trail: Rc<RefCell<Trail>>,
    }

    fn strict(stat: &Stat) -> bool {
        stat.st_uid == geteuid().as_raw() && stat.st_mode & 0o022 == 0
    }

    impl Dir {
        fn root(path: &Path) -> Stage<Self> {
            let fd = open_dir(path).ok_or("config")?;
            Self::admit(fd, Vec::new(), Rc::default(), "config")
        }

        fn dir(&self, name: &str, stage: &'static str) -> Stage<Self> {
            let fd = fs::openat(&self.fd, name, DIR, Mode::empty()).map_err(|_| stage)?;
            let mut names = self.names.clone();
            names.push(name.to_owned());
            Self::admit(fd, names, Rc::clone(&self.trail), stage)
        }

        fn admit(
            fd: OwnedFd,
            names: Vec<String>,
            trail: Rc<RefCell<Trail>>,
            stage: &'static str,
        ) -> Stage<Self> {
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::Directory && strict(&stat),
                stage,
            )?;
            trail
                .borrow_mut()
                .0
                .push((names.clone(), false, (stat.st_dev, stat.st_ino)));
            Ok(Self { fd, names, trail })
        }

        fn file(&self, name: &str, limit: u64, stage: &'static str) -> Stage<(File, Stat)> {
            let fd = fs::openat(&self.fd, name, FILE, Mode::empty()).map_err(|_| stage)?;
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                    && stat.st_nlink == 1
                    && strict(&stat)
                    && u64::try_from(stat.st_size).is_ok_and(|size| size <= limit),
                stage,
            )?;
            let mut names = self.names.clone();
            names.push(name.to_owned());
            self.trail
                .borrow_mut()
                .0
                .push((names, true, (stat.st_dev, stat.st_ino)));
            Ok((File::from(fd), stat))
        }

        fn read(&self, name: &str, limit: u64, stage: &'static str) -> Stage<Vec<u8>> {
            let (file, stat) = self.file(name, limit, stage)?;
            Ok(read_tail(&file, &stat, limit, stage)?.0)
        }
    }

    /// Walks every recorded name again from the config root without following
    /// links; a renamed or replaced ancestor or file no longer reaches its inode.
    fn same_names(config: &Path, trail: &Trail) -> Stage<()> {
        let root = open_dir(config).ok_or("path-changed")?;
        for (names, file, ident) in &trail.0 {
            let mut fd = None;
            for (depth, name) in names.iter().enumerate() {
                let flags = if *file && depth + 1 == names.len() {
                    FILE
                } else {
                    DIR
                };
                let next = fs::openat(fd.as_ref().unwrap_or(&root), name, flags, Mode::empty())
                    .map_err(|_| "path-changed")?;
                fd = Some(next);
            }
            let now = fs::fstat(fd.as_ref().unwrap_or(&root)).map_err(|_| "path-changed")?;
            let kind = FileType::from_raw_mode(now.st_mode);
            gate(
                (now.st_dev, now.st_ino) == *ident
                    && strict(&now)
                    && if *file {
                        kind == FileType::RegularFile && now.st_nlink == 1
                    } else {
                        kind == FileType::Directory
                    },
                "path-changed",
            )?;
        }
        Ok(())
    }

    /// Reads at most `window` trailing bytes plus one probe byte on each side
    /// and proves the file kept the version it had when opened. `true` means
    /// the window starts mid-file, so its first line is not a whole record.
    fn read_tail(
        file: &File,
        stat: &Stat,
        window: u64,
        stage: &'static str,
    ) -> Stage<(Vec<u8>, bool)> {
        let size = u64::try_from(stat.st_size).map_err(|_| stage)?;
        let recent = size > window;
        // The extra leading byte shows whether the window starts on a record boundary.
        let offset = if recent { size - window - 1 } else { 0 };
        let len = usize::try_from(size - offset).map_err(|_| stage)?;
        let mut reader = file;
        reader.seek(SeekFrom::Start(offset)).map_err(|_| stage)?;
        let mut body = Vec::with_capacity(len + 1);
        reader
            .take(len as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| stage)?;
        gate(
            body.len() == len && fs::fstat(file).is_ok_and(|now| unchanged(stat, &now)),
            stage,
        )?;
        Ok((body, recent))
    }

    /// Complete records only; `None` marks one over the line cap or not JSON.
    /// A body whose last line lacks its newline is a write in progress: `partial`.
    fn records(body: &[u8], recent: bool, partial: &'static str) -> Stage<Vec<Option<Value>>> {
        gate(body.last().is_none_or(|&byte| byte == b'\n'), partial)?;
        let start = if recent {
            body.iter()
                .position(|&byte| byte == b'\n')
                .map_or(body.len(), |newline| newline + 1)
        } else {
            0
        };
        Ok(body[start..]
            .split_inclusive(|&byte| byte == b'\n')
            .filter_map(|line| line.strip_suffix(b"\n"))
            .map(|line| oms_panel_core::decode(line, LINE_LIMIT).ok())
            .collect())
    }

    /// The Turn a human-typed managed task names, optionally after one pasted marker.
    pub(super) fn managed_turn(entry: &Value) -> Option<&str> {
        if entry["type"] != "user"
            || entry["promptSource"] != "typed"
            || entry["origin"]["kind"] != "human"
        {
            return None;
        }
        let mut prompt = entry["message"]["content"].as_str()?;
        if let Some(pasted) = prompt.strip_prefix("<pasted_content id=\"") {
            let (id, rest) = pasted.split_once("\">")?;
            if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            prompt = rest.trim_start();
        }
        let assignment = prompt.strip_prefix(TASK_PREFIX)?.strip_prefix(" Turn ")?;
        let (turn, rest) = assignment.split_at_checked(20)?;
        (turn
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && rest.starts_with(". You own this provider's work."))
        .then_some(turn)
    }

    /// A lead `user` entry after the Turn that provably did not come from a person:
    /// only the machine shapes Claude Code writes pass (a human origin of any
    /// `promptSource`, unknown origin kinds and mixed arrays all stay people).
    fn machine_entry(entry: &Value) -> bool {
        let system = entry["promptSource"] == "system";
        let meta = entry["isMeta"] == true;
        let content = &entry["message"]["content"];
        match entry.get("origin") {
            None => {
                (meta && (system || entry.get("promptSource").is_none()))
                    || entry["isCompactSummary"] == true
                    || (entry.get("promptSource").is_none()
                        && content.as_array().is_some_and(|blocks| {
                            !blocks.is_empty()
                                && blocks.iter().all(|block| block["type"] == "tool_result")
                        }))
            }
            Some(origin) => match origin["kind"].as_str() {
                Some("task-notification") => {
                    system
                        && !meta
                        && content
                            .as_str()
                            .is_some_and(|text| text.starts_with("<task-notification>\n"))
                }
                Some("peer" | "auto-continuation") => system && meta,
                _ => false,
            },
        }
    }

    /// A key seen twice (a reused tool id, a doubly bound agent) binds nothing.
    fn once<K: Ord, V>(map: &mut BTreeMap<K, Option<V>>, key: K, value: V) {
        map.entry(key)
            .and_modify(|seen| *seen = None)
            .or_insert(Some(value));
    }

    /// Session ids name a file and a directory, so only the canonical UUID shape passes.
    fn uuid(id: &str) -> bool {
        id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
    }

    fn agent_id(id: &str) -> bool {
        (8..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Claude Code's project folder name: every non-alphanumeric becomes `-`.
    fn project_key(repo: &Path) -> Stage<String> {
        let key: String = repo
            .to_str()
            .ok_or("project-key")?
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        gate(!key.is_empty() && key.len() <= 255, "project-key")?;
        Ok(key)
    }

    /// A row's `timestamp` when it parses as RFC 3339, kept as recorded.
    fn stamp(entry: &Value) -> Option<(&str, chrono::DateTime<chrono::FixedOffset>)> {
        let raw = entry["timestamp"].as_str()?;
        Some((raw, chrono::DateTime::parse_from_rfc3339(raw).ok()?))
    }

    /// Lead-observed spawn and completion clocks of one child; `None` is unknown.
    type Clocks = (Option<String>, Option<String>);

    /// The spawn is the `Agent` call's row; the completion is the exact notice
    /// that makes the state "done", known only when every such notice comes
    /// after the call and names one instant no earlier than a known spawn.
    fn clocks(call: Option<&Value>, called: usize, done: &[(usize, &Value)]) -> Clocks {
        let spawned = call.and_then(stamp);
        let mut ends = done.iter().map(|&(at, entry)| {
            stamp(entry)
                .filter(|(_, end)| at > called && spawned.is_none_or(|(_, start)| *end >= start))
        });
        let first = ends.next().flatten();
        let completed =
            first.filter(|(_, end)| ends.all(|other| other.is_some_and(|(_, e)| e == *end)));
        (
            spawned.map(|(raw, _)| raw.to_owned()),
            completed.map(|(raw, _)| raw.to_owned()),
        )
    }

    pub(super) struct Scan {
        pub(super) turn: String,
        pub(super) children: Vec<ClaudeChild>,
        /// agentId -> clocks, for exactly the children above.
        clocks: BTreeMap<String, Clocks>,
        transcript: (File, Stat),
        sessions: Dir,
        record_name: String,
        record: Vec<u8>,
        subagents: Option<Dir>,
        config: PathBuf,
        trail: Rc<RefCell<Trail>>,
    }

    impl Scan {
        /// Claude can append or switch its current session while keeping the process.
        pub(super) fn confirm(&self) -> Stage<()> {
            let (file, stat) = &self.transcript;
            gate(
                fs::fstat(file).is_ok_and(|now| unchanged(stat, &now)),
                "lead-changed",
            )?;
            let record = self
                .sessions
                .read(&self.record_name, RECORD_LIMIT, "session-record")?;
            gate(record == self.record, "session-switched")?;
            same_names(&self.config, &self.trail.borrow())
        }
    }

    /// `turn` of `None` accepts the last managed Turn, whichever it is.
    pub(super) fn scan(lead: &Lead, repo: &Path, turn: Option<&str>) -> Stage<Scan> {
        let config = Dir::root(lead.config)?;
        let trail = Rc::clone(&config.trail);
        let sessions = config.dir("sessions", "sessions")?;
        let record_name = format!("{}.json", lead.pid);
        let record = sessions.read(&record_name, RECORD_LIMIT, "session-record")?;
        let meta =
            oms_panel_core::decode(&record, RECORD_LIMIT as usize).map_err(|_| "session-record")?;
        let session = text(&meta, "sessionId");
        gate(
            meta["pid"].as_u64() == Some(u64::from(lead.pid))
                && meta["procStart"]
                    .as_str()
                    .is_some_and(|start| Some(start) == start_time(lead.birth))
                && uuid(session),
            "session-record",
        )?;
        gate(
            Path::new(text(&meta, "cwd")).canonicalize().ok().as_deref() == Some(repo),
            "session-cwd",
        )?;
        let project = config
            .dir("projects", "projects")?
            .dir(&project_key(repo)?, "project")?;
        let transcript = project.file(
            &format!("{session}.jsonl"),
            TRANSCRIPT_LIMIT,
            "lead-transcript",
        )?;
        let (body, recent) = read_tail(&transcript.0, &transcript.1, LEAD_WINDOW, "lead-read")?;
        let entries = records(&body, recent, "lead-partial")?;
        let start = entries
            .iter()
            .rposition(|entry| entry.as_ref().and_then(managed_turn).is_some())
            .ok_or("no-turn")?;
        let last = entries[start].as_ref().and_then(managed_turn);
        gate(turn.is_none() || last == turn, "stale-turn")?;
        let turn = last.ok_or("no-turn")?;
        // Anything unreadable after the Turn could hide a newer Turn or a child call.
        let current = &entries[start..];
        gate(current.iter().all(Option::is_some), "lead-line")?;
        // The Turn must be the latest human message of the lead: a later one
        // may have redirected or cancelled it. Unknown user shapes fail closed.
        gate(
            current[1..]
                .iter()
                .flatten()
                .filter(|entry| entry["type"] == "user")
                .all(machine_entry),
            "newer-human",
        )?;
        gate(
            current
                .iter()
                .flatten()
                .filter(|entry| matches!(text(entry, "type"), "user" | "assistant"))
                .all(|entry| entry["isSidechain"] == false),
            "lead-sidechain",
        )?;
        gate(
            current
                .iter()
                .flatten()
                .all(|entry| entry["sessionId"] == session),
            "turn-session",
        )?;

        let mut calls = BTreeMap::new();
        let mut results = BTreeMap::new();
        let mut sends = BTreeSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(entry) = entry.as_ref().filter(|e| e["sessionId"] == session) else {
                continue;
            };
            let blocks = rows(&entry["message"]["content"]);
            match text(entry, "type") {
                "assistant" => {
                    for block in blocks.iter().filter(|b| b["type"] == "tool_use") {
                        let input = &block["input"];
                        match (text(block, "name"), input["prompt"].as_str()) {
                            ("Agent", Some(prompt)) => {
                                let title = text(input, "description");
                                once(&mut calls, text(block, "id"), (index, prompt, title));
                            }
                            ("SendMessage", _) if index > start => {
                                sends.insert(text(input, "to"));
                            }
                            _ => {}
                        }
                    }
                }
                "user" => {
                    let agent = text(&entry["toolUseResult"], "agentId");
                    let mut answered = blocks.iter().filter(|b| b["type"] == "tool_result");
                    if let (Some(block), None) = (answered.next(), answered.next())
                        && agent_id(agent)
                    {
                        once(&mut results, text(block, "tool_use_id"), (index, agent));
                    }
                }
                _ => {}
            }
        }
        // agentId -> (call index, Agent tool id, prompt, description, result index).
        let mut agents = BTreeMap::new();
        for (tool, result) in results {
            let (Some((answered, agent)), Some(Some((called, prompt, title)))) =
                (result, calls.get(tool).copied())
            else {
                continue;
            };
            if answered > called {
                once(&mut agents, agent, (called, tool, prompt, title, answered));
            }
        }
        // Only this Turn's own spawns, and none the lead later wrote to: a
        // message cannot be told from a reply without trusting the wrapper.
        let mut found: Vec<_> = agents
            .into_iter()
            .filter_map(|(agent, bound)| {
                let (called, tool, prompt, title, answered) = bound?;
                (called > start && !sends.contains(agent))
                    .then_some((answered, called, agent, tool, prompt, title))
            })
            .collect();
        // Same roster as the whole-file collector: the most recently answered,
        // chosen before any metadata read and never refilled from older ones.
        found.sort_by_key(|&(answered, ..)| std::cmp::Reverse(answered));
        found.truncate(CLAUDE_ROSTER_LIMIT);
        found.sort_by_key(|&(_, called, agent, ..)| (called, agent));
        // (lead index, entry, content) of this Turn's task notifications.
        let notices: Vec<(usize, &Value, &str)> = current
            .iter()
            .enumerate()
            .filter_map(|(offset, entry)| Some((start + offset, entry.as_ref()?)))
            .filter(|(_, entry)| {
                entry["type"] == "user" && entry["origin"]["kind"] == "task-notification"
            })
            .filter_map(|(at, entry)| Some((at, entry, entry["message"]["content"].as_str()?)))
            .collect();

        let mut children = Vec::new();
        let mut times = BTreeMap::new();
        let subagents = if found.is_empty() {
            None
        } else {
            Some(
                project
                    .dir(session, "session-dir")?
                    .dir("subagents", "subagents")?,
            )
        };
        if let Some(dir) = &subagents {
            for (_, called, agent, tool, prompt, title) in found {
                #[cfg(test)]
                META_READS.with(|n| n.set(n.get() + 1));
                // A child whose metadata cannot be proven is left out, not shown.
                let Ok(meta) = dir.read(
                    &format!("agent-{agent}.meta.json"),
                    RECORD_LIMIT,
                    "child-meta",
                ) else {
                    continue;
                };
                if !oms_panel_core::decode(&meta, RECORD_LIMIT as usize)
                    .is_ok_and(|meta| meta["toolUseId"] == tool && meta["spawnDepth"] == 1)
                {
                    continue;
                }
                let Ok((_, stat)) = dir.file(
                    &format!("agent-{agent}.jsonl"),
                    TRANSCRIPT_LIMIT,
                    "child-transcript",
                ) else {
                    continue;
                };
                let done: Vec<_> = notices
                    .iter()
                    .filter(|(_, _, notice)| claude_completion(notice, agent, tool))
                    .map(|&(at, entry, _)| (at, entry))
                    .collect();
                let call = entries[called].as_ref();
                times.insert(agent.to_owned(), clocks(call, called, &done));
                children.push(ClaudeChild {
                    locator: ClaudeChildLocator {
                        pid: lead.pid,
                        birth: lead.birth.to_owned(),
                        session: session.to_owned(),
                        turn: turn.to_owned(),
                        agent: agent.to_owned(),
                        tool_use: tool.to_owned(),
                        boundary: digest(prompt.as_bytes()),
                        lead: (transcript.1.st_dev, transcript.1.st_ino),
                        child: (stat.st_dev, stat.st_ino),
                    },
                    title: title.chars().take(80).collect(),
                    state: if done.is_empty() { "unknown" } else { "done" },
                    usage: None,
                });
            }
        }
        Ok(Scan {
            turn: turn.to_owned(),
            children,
            clocks: times,
            transcript,
            sessions,
            record_name,
            record,
            subagents,
            config: lead.config.to_path_buf(),
            trail,
        })
    }

    /// `between` runs after the reads and before the rechecks (a test seam).
    pub(super) fn chat(
        lead: &Lead,
        repo: &Path,
        turn: &str,
        expected: &ClaudeChildLocator,
        between: impl FnOnce(),
    ) -> Stage<String> {
        gate(
            lead.birth == expected.birth && turn == expected.turn,
            "locator",
        )?;
        let scan = scan(lead, repo, Some(turn))?;
        gate(
            scan.children.iter().any(|child| child.locator == *expected),
            "locator",
        )?;
        let (history, _) = read_child(&scan, expected, between)?;
        scan.confirm()?;
        chat_tail(&history).ok_or("no-reply")
    }

    /// One child's usage from its own tail and the scan's lead clocks; the
    /// caller confirms the scan after it.
    fn usage(scan: &Scan, expected: &ClaudeChildLocator) -> Stage<ClaudeChildUsage> {
        let (_, response) = read_child(scan, expected, || {})?;
        let (spawned_at, delivered_at) =
            scan.clocks.get(&expected.agent).cloned().ok_or("locator")?;
        Ok(ClaudeChildUsage {
            response,
            spawned_at,
            delivered_at,
        })
    }

    /// The child transcript the scan proved, read once within `CHILD_WINDOW`.
    fn read_child(
        scan: &Scan,
        expected: &ClaudeChildLocator,
        between: impl FnOnce(),
    ) -> Stage<(String, Option<ClaudeResponse>)> {
        let dir = scan.subagents.as_ref().ok_or("locator")?;
        let (file, stat) = dir.file(
            &format!("agent-{}.jsonl", expected.agent),
            TRANSCRIPT_LIMIT,
            "child-transcript",
        )?;
        gate(
            (stat.st_dev, stat.st_ino) == expected.child,
            "child-replaced",
        )?;
        let (body, recent) = read_tail(&file, &stat, CHILD_WINDOW, "child-read")?;
        let found = replies(&body, recent, expected)?;
        between();
        gate(
            fs::fstat(&file).is_ok_and(|now| unchanged(&stat, &now)),
            "child-changed",
        )?;
        Ok(found)
    }

    /// Counters and timestamp of one row; `None` marks a row without four
    /// non-negative integer counters or an RFC 3339 timestamp.
    type Claim<'a> = Option<([u64; 4], &'a str)>;

    /// One assistant row's claim about the response its `message.id` names.
    fn claim(entry: &Value) -> Option<(&str, Claim<'_>)> {
        let id = entry["message"]["id"].as_str().filter(|id| {
            (1..=128).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })?;
        let usage = &entry["message"]["usage"];
        let mut counts = [0; 4];
        for (count, key) in counts.iter_mut().zip(USAGE_KEYS) {
            match usage[key].as_u64() {
                Some(value) => *count = value,
                None => return Some((id, None)),
            }
        }
        Some((id, stamp(entry).map(|(raw, _)| (counts, raw))))
    }

    /// Rows of one `message.id` seen within the segment.
    struct Group {
        last: usize,
        usage: Option<[u64; 4]>,
        at: String,
        malformed: bool,
        conflict: bool,
    }

    /// Streaming rows repeat one response's id and counters, so they count once,
    /// timed by its last row. Only the response of the newest assistant row
    /// counts: a bad id, clock or counter there, or differing counters under
    /// its id, leave it unknown rather than letting an older response pose as latest.
    fn latest(groups: BTreeMap<String, Group>, newest: Option<usize>) -> Option<ClaudeResponse> {
        let (id, group) = groups
            .into_iter()
            .find(|(_, group)| Some(group.last) == newest)?;
        (!group.malformed && !group.conflict).then_some(ClaudeResponse {
            id,
            at: group.at,
            usage: group.usage?,
        })
    }

    /// The child's own text and handback replies after the single spawn-prompt
    /// entry, up to the next message it received; both kinds keep file order.
    /// Any other string user entry, including a coordinator wrapper, ends the segment.
    /// Usage comes from the segment's assistant rows only: a later response
    /// answers another message, so it is never this spawn's latest.
    fn replies(
        body: &[u8],
        recent: bool,
        expected: &ClaudeChildLocator,
    ) -> Stage<(String, Option<ClaudeResponse>)> {
        let mut matched = false;
        let mut collecting = false;
        let mut history = Vec::new();
        let mut groups = BTreeMap::<String, Group>::new();
        let mut newest = None;
        for (position, entry) in records(body, recent, "child-partial")?
            .into_iter()
            .enumerate()
        {
            let entry = entry.ok_or("child-line")?;
            let kind = text(&entry, "type");
            if kind != "user" && kind != "assistant" {
                continue;
            }
            gate(
                entry["agentId"] == expected.agent.as_str()
                    && entry["sessionId"] == expected.session.as_str()
                    && entry["isSidechain"] == true,
                "child-entry",
            )?;
            let content = &entry["message"]["content"];
            if kind == "user" {
                if let Some(message) = content.as_str() {
                    collecting = entry.get("isMeta").is_none_or(|meta| *meta == false)
                        && digest(message.as_bytes()) == expected.boundary;
                    gate(!(collecting && matched), "ambiguous-boundary")?;
                    matched |= collecting;
                } else {
                    // Only non-empty tool results are neither a boundary nor shown;
                    // any other array may be a message the child received.
                    gate(
                        content.as_array().is_some_and(|blocks| {
                            !blocks.is_empty()
                                && blocks.iter().all(|block| block["type"] == "tool_result")
                        }),
                        "child-entry",
                    )?;
                }
                continue;
            }
            if !collecting {
                continue;
            }
            newest = Some(position);
            if let Some((id, row)) = claim(&entry) {
                let group = groups.entry(id.to_owned()).or_insert(Group {
                    last: position,
                    usage: None,
                    at: String::new(),
                    malformed: false,
                    conflict: false,
                });
                group.last = position;
                match row {
                    None => group.malformed = true,
                    Some((usage, at)) => {
                        group.conflict |= group.usage.is_some_and(|seen| seen != usage);
                        group.usage = Some(usage);
                        at.clone_into(&mut group.at);
                    }
                }
            }
            for block in rows(content) {
                let reply = match (text(block, "type"), text(block, "name")) {
                    ("text", _) => block["text"].as_str(),
                    ("tool_use", "SubagentHandback") => block["input"]["message"].as_str(),
                    _ => None,
                };
                if let Some(reply) = reply.filter(|reply| !reply.trim().is_empty()) {
                    history.push(reply.to_owned());
                }
            }
        }
        gate(matched, "no-boundary")?;
        Ok((history.join("\n\n"), latest(groups, newest)))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{
        CHILD_WINDOW, LEAD_WINDOW, LINE_LIMIT, Lead, META_READS, Stage, USAGE_TAILS, chat, current,
        managed_turn, proven, scan,
    };
    use super::{
        ClaudeChild, ClaudeChildLocator, claude_board_children, claude_child_chat,
        claude_current_children,
    };
    use crate::claude_lead_usage::linux::start_time;
    use crate::native_children::TASK_PREFIX;
    use crate::orchestrator::claude_usage_field;
    use oms_panel_core::digest;
    use serde_json::{Value, json};
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    const PID: u32 = 4242;
    const BIRTH: &str = "fixture-boot:987654";
    const SESSION: &str = "0a1b2c3d-4e5f-6789-abcd-ef0123456789";
    const OTHER_SESSION: &str = "1a1b2c3d-4e5f-6789-abcd-ef0123456789";
    const TURN: &str = "1234567890abcdef1234";
    const STALE: &str = "abcdef1234567890abcd";
    const NEWER: &str = "00000000000000000001";
    const AGENT: &str = "a1b2c3d4e5f60718";
    const OLD_AGENT: &str = "f0e1d2c3b4a59687";
    const TOOL: &str = "toolu_fixture_new";
    const OLD_TOOL: &str = "toolu_fixture_old";
    const PROMPT: &str = "fixture child prompt";
    const OLD_PROMPT: &str = "fixture old prompt";
    const CONTINUE: &str = "fixture continue";
    const FOLLOW: &str = "fixture follow up";
    /// Rule: every text block and SubagentHandback message after the boundary, in file order.
    const OWN: &str = "first reply\n\nsecond reply\n\nhandback note";

    fn turn_prompt(turn: &str) -> String {
        format!("{TASK_PREFIX} Turn {turn}. You own this provider's work. Fixture body")
    }

    fn typed(content: &str) -> Value {
        json!({"type": "user", "sessionId": SESSION, "isSidechain": false,
               "promptSource": "typed", "origin": {"kind": "human"}, "message": {"role": "user", "content": content}})
    }

    fn assistant(blocks: Value) -> Value {
        json!({"type": "assistant", "sessionId": SESSION, "isSidechain": false,
               "message": {"role": "assistant", "content": blocks}})
    }

    fn agent_call(tool: &str, prompt: &str) -> Value {
        assistant(
            json!([{"type": "tool_use", "name": "Agent", "id": tool, "input": {
            "prompt": prompt, "description": "fixture", "subagent_type": "general-purpose",
            "run_in_background": true}}]),
        )
    }

    fn spawned(tool: &str, agent: &str) -> Value {
        json!({"type": "user", "sessionId": SESSION, "isSidechain": false,
               "message": {"role": "user", "content": [
                  {"type": "tool_result", "tool_use_id": tool, "content": "tool result hidden"}]},
               "toolUseResult": {"agentId": agent,
                  "content": [{"type": "text", "text": "inherited result hidden"}]}})
    }

    fn send(to: &str, message: &str) -> Value {
        assistant(
            json!([{"type": "tool_use", "name": "SendMessage", "id": "toolu_send",
                          "input": {"to": to, "message": message}}]),
        )
    }

    fn child_user(agent: &str, content: Value) -> Value {
        json!({"type": "user", "sessionId": SESSION, "agentId": agent, "isSidechain": true,
               "message": {"role": "user", "content": content}})
    }

    /// A `SendMessage` continuation as Claude Code 2.1.327 records it in the child.
    fn wrapped(agent: &str, message: &str) -> Value {
        let mut entry = child_user(
            agent,
            json!(format!(
                "The coordinator sent a message while you were working:\n{message}\n\nAddress this before completing your current task."
            )),
        );
        entry["isMeta"] = json!(true);
        entry["origin"] = json!({"kind": "coordinator"});
        entry
    }

    fn child_reply(agent: &str, blocks: Value) -> Value {
        let mut entry = assistant(blocks);
        entry["agentId"] = json!(agent);
        entry["isSidechain"] = json!(true);
        entry
    }

    /// A response row as Claude Code records it: id, clock and usage counters.
    fn billed(mut entry: Value, id: &str, at: &str, usage: [i64; 4]) -> Value {
        let [input, output, creation, read] = usage;
        entry["message"]["id"] = json!(id);
        entry["message"]["usage"] = json!({"input_tokens": input, "output_tokens": output,
            "cache_creation_input_tokens": creation, "cache_read_input_tokens": read,
            "service_tier": "standard"});
        stamped(entry, at)
    }

    fn stamped(mut entry: Value, at: &str) -> Value {
        entry["timestamp"] = json!(at);
        entry
    }

    fn padded(mut entry: Value, bytes: usize) -> Value {
        entry["padding"] = json!("x".repeat(bytes));
        entry
    }

    fn mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    struct Fixture {
        base: PathBuf,
        config: PathBuf,
        repo: PathBuf,
        lead: Vec<Value>,
        child: Vec<Value>,
        old: Vec<Value>,
        meta: Value,
        record: Value,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("oms-claude-child-{}-{label}", std::process::id()));
            fs::create_dir(&base).unwrap();
            mode(&base, 0o700);
            let repo = base.join("repo");
            fs::create_dir(&repo).unwrap();
            let fixture = Self {
                config: base.join("config"),
                lead: vec![
                    typed(&turn_prompt(STALE)),
                    agent_call(OLD_TOOL, OLD_PROMPT),
                    spawned(OLD_TOOL, OLD_AGENT),
                    typed(&turn_prompt(TURN)),
                    assistant(json!([{"type": "text", "text": "parent narration hidden"}])),
                    agent_call(TOOL, PROMPT),
                    spawned(TOOL, AGENT),
                    json!({"type": "user", "sessionId": SESSION, "isSidechain": false,
                        "origin": {"kind": "task-notification"}, "promptSource": "system",
                        "message": {"role": "user",
                        "content": "<task-notification>\n<result>notification hidden</result></task-notification>"}}),
                    send(OLD_AGENT, CONTINUE),
                ],
                child: vec![
                    child_user(AGENT, json!(PROMPT)),
                    child_reply(
                        AGENT,
                        json!([{"type": "text", "text": "first reply"},
                               {"type": "tool_use", "name": "Read", "id": "toolu_read",
                                "input": {"file_path": "fixture hidden"}}]),
                    ),
                    child_user(
                        AGENT,
                        json!([{"type": "tool_result", "tool_use_id": "toolu_read",
                                "content": "tool output hidden"}]),
                    ),
                    child_reply(
                        AGENT,
                        json!([{"type": "thinking", "thinking": "thinking hidden"},
                               {"type": "text", "text": "second reply"},
                               {"type": "tool_use", "name": "SubagentHandback", "id": "toolu_hand",
                                "input": {"message": "handback note"}}]),
                    ),
                ],
                old: vec![
                    child_user(OLD_AGENT, json!(OLD_PROMPT)),
                    child_reply(
                        OLD_AGENT,
                        json!([{"type": "text", "text": "earlier turn hidden"}]),
                    ),
                    wrapped(OLD_AGENT, CONTINUE),
                    child_reply(
                        OLD_AGENT,
                        json!([{"type": "text", "text": "continued answer"}]),
                    ),
                ],
                meta: json!({"agentType": "general-purpose", "toolUseId": TOOL, "spawnDepth": 1}),
                record: json!({"pid": PID, "procStart": "987654", "sessionId": SESSION,
                               "cwd": repo.to_str().unwrap()}),
                repo,
                base,
            };
            fixture.save();
            fixture
        }

        fn project(&self) -> PathBuf {
            let key: String = self
                .repo
                .to_str()
                .unwrap()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            self.config.join("projects").join(key)
        }

        fn subagents(&self) -> PathBuf {
            self.project().join(SESSION).join("subagents")
        }

        fn lead_path(&self) -> PathBuf {
            self.project().join(format!("{SESSION}.jsonl"))
        }

        fn child_path(&self, agent: &str) -> PathBuf {
            self.subagents().join(format!("agent-{agent}.jsonl"))
        }

        fn meta_path(&self, agent: &str) -> PathBuf {
            self.subagents().join(format!("agent-{agent}.meta.json"))
        }

        fn record_path(&self) -> PathBuf {
            self.config.join(format!("sessions/{PID}.json"))
        }

        /// Strict modes: owned by the caller, no group or other write anywhere.
        fn save(&self) {
            let project = self.project();
            for (dir, bits) in [
                (self.config.clone(), 0o700),
                (self.config.join("sessions"), 0o700),
                (self.config.join("projects"), 0o750),
                (project.clone(), 0o700),
                (project.join(SESSION), 0o755),
                (self.subagents(), 0o755),
            ] {
                fs::create_dir_all(&dir).unwrap();
                mode(&dir, bits);
            }
            let old_meta = json!({"toolUseId": OLD_TOOL, "spawnDepth": 1});
            for (path, body, bits) in [
                (self.record_path(), self.record.to_string(), 0o644),
                (self.lead_path(), lines(&self.lead), 0o600),
                (self.child_path(AGENT), lines(&self.child), 0o600),
                (self.child_path(OLD_AGENT), lines(&self.old), 0o600),
                (self.meta_path(AGENT), self.meta.to_string(), 0o644),
                (self.meta_path(OLD_AGENT), old_meta.to_string(), 0o644),
            ] {
                fs::write(&path, body).unwrap();
                mode(&path, bits);
            }
        }

        fn lead(&self) -> Lead<'_> {
            Lead::new(PID, BIRTH, &self.config)
        }

        fn rows(&self, turn: Option<&str>) -> Stage<Vec<ClaudeChild>> {
            let scan = scan(&self.lead(), &self.repo, turn)?;
            scan.confirm()?;
            Ok(scan.children)
        }

        fn children(&self, turn: &str) -> Stage<Vec<ClaudeChildLocator>> {
            Ok(self
                .rows(Some(turn))?
                .into_iter()
                .map(|child| child.locator)
                .collect())
        }

        fn locator(&self, agent: &str) -> ClaudeChildLocator {
            let children = self.children(TURN).unwrap();
            children.into_iter().find(|c| c.agent == agent).unwrap()
        }

        fn chat(&self, expected: &ClaudeChildLocator) -> Stage<String> {
            chat(&self.lead(), &self.repo, TURN, expected, || {})
        }

        /// The Board's usage field for `expected`, through the Board's own read.
        fn telemetry(&self, expected: &ClaudeChildLocator) -> Stage<Value> {
            let (_, children) = current(&self.lead(), &self.repo, USAGE_TAILS, || {})?;
            let child = children.into_iter().find(|c| c.locator == *expected);
            Ok(claude_usage_field(
                &child.ok_or("locator")?.usage.ok_or("withheld")?,
            ))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn lines(rows: &[Value]) -> String {
        rows.iter().map(|row| format!("{row}\n")).collect()
    }

    fn append(path: &Path, row: &Value) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{row}").unwrap();
    }

    #[test]
    fn child_chat_requires_the_full_proof_chain() {
        type Case = (&'static str, Box<dyn Fn(&mut Fixture)>, Stage<&'static str>);
        let edit = |change: fn(&mut Fixture)| -> Box<dyn Fn(&mut Fixture)> {
            Box::new(move |f: &mut Fixture| {
                change(f);
                f.save();
            })
        };
        let fs_only = |change: fn(&Fixture)| -> Box<dyn Fn(&mut Fixture)> {
            Box::new(move |f: &mut Fixture| change(f))
        };
        let modes = |changes: &'static [(&'static str, u32)]| -> Box<dyn Fn(&mut Fixture)> {
            Box::new(move |f: &mut Fixture| {
                for (part, bits) in changes {
                    let path = match *part {
                        "config" => f.config.clone(),
                        "sessions" => f.config.join("sessions"),
                        "projects" => f.config.join("projects"),
                        "project" => f.project(),
                        "session" => f.project().join(SESSION),
                        "subagents" => f.subagents(),
                        "record" => f.record_path(),
                        "lead" => f.lead_path(),
                        "child" => f.child_path(AGENT),
                        "meta" => f.meta_path(AGENT),
                        _ => unreachable!(),
                    };
                    mode(&path, *bits);
                }
            })
        };
        let cases: Vec<Case> = vec![
            ("own-result", edit(|_| {}), Ok(OWN)),
            (
                "pasted-turn",
                edit(|f| {
                    f.lead[3] = typed(&format!(
                        "<pasted_content id=\"9f3a\">\n {}",
                        turn_prompt(TURN)
                    ))
                }),
                Ok(OWN),
            ),
            (
                "preceded-turn",
                edit(|f| f.lead[3] = typed(&format!("note {}", turn_prompt(TURN)))),
                Err("stale-turn"),
            ),
            (
                "newer-turn",
                edit(|f| f.lead.push(typed(&turn_prompt(NEWER)))),
                Err("stale-turn"),
            ),
            (
                "reused-pid",
                edit(|f| f.record["procStart"] = json!("987655")),
                Err("session-record"),
            ),
            (
                "other-cwd",
                edit(|f| f.record["cwd"] = json!(f.base.to_str().unwrap())),
                Err("session-cwd"),
            ),
            (
                "lead-entry-session",
                edit(|f| f.lead[5]["sessionId"] = json!(OTHER_SESSION)),
                Err("turn-session"),
            ),
            (
                "lead-oversize-line",
                edit(|f| f.lead.push(padded(assistant(json!([])), LINE_LIMIT))),
                Err("lead-line"),
            ),
            (
                "child-entry-session",
                edit(|f| f.child[1]["sessionId"] = json!(OTHER_SESSION)),
                Err("child-entry"),
            ),
            (
                "child-entry-agent",
                edit(|f| f.child[1]["agentId"] = json!(OLD_AGENT)),
                Err("child-entry"),
            ),
            (
                "not-sidechain",
                edit(|f| f.child[3]["isSidechain"] = json!(false)),
                Err("child-entry"),
            ),
            (
                "meta-tool-mismatch",
                edit(|f| f.meta["toolUseId"] = json!(OLD_TOOL)),
                Err("locator"),
            ),
            (
                "boundary-twice",
                edit(|f| f.child.push(child_user(AGENT, json!(PROMPT)))),
                Err("ambiguous-boundary"),
            ),
            (
                "meta-prompt",
                edit(|f| f.child[0]["isMeta"] = json!(true)),
                Err("no-boundary"),
            ),
            (
                "no-boundary",
                edit(|f| f.child[0] = child_user(AGENT, json!("other prompt"))),
                Err("no-boundary"),
            ),
            (
                "child-oversize-line",
                edit(|f| {
                    f.child
                        .push(padded(child_reply(AGENT, json!([])), LINE_LIMIT))
                }),
                Err("child-line"),
            ),
            (
                "symlinked-subagents",
                fs_only(|f| {
                    let real = f.base.join("real-subagents");
                    fs::rename(f.subagents(), &real).unwrap();
                    symlink(&real, f.subagents()).unwrap();
                }),
                Err("subagents"),
            ),
            (
                "symlinked-child",
                fs_only(|f| {
                    let real = f.base.join("real.jsonl");
                    fs::rename(f.child_path(AGENT), &real).unwrap();
                    symlink(&real, f.child_path(AGENT)).unwrap();
                }),
                Err("locator"),
            ),
            (
                "hard-linked-child",
                fs_only(|f| fs::hard_link(f.child_path(AGENT), f.base.join("twin")).unwrap()),
                Err("locator"),
            ),
            (
                "hard-linked-lead",
                fs_only(|f| fs::hard_link(f.lead_path(), f.base.join("twin")).unwrap()),
                Err("lead-transcript"),
            ),
            (
                "replaced-child",
                fs_only(|f| {
                    let copy = f.base.join("copy.jsonl");
                    fs::copy(f.child_path(AGENT), &copy).unwrap();
                    fs::rename(&copy, f.child_path(AGENT)).unwrap();
                }),
                Err("locator"),
            ),
            (
                "group-writable-lead",
                modes(&[("lead", 0o660)]),
                Err("lead-transcript"),
            ),
            (
                "group-writable-child",
                modes(&[("child", 0o660)]),
                Err("locator"),
            ),
            (
                "group-writable-projects",
                modes(&[("projects", 0o770)]),
                Err("projects"),
            ),
            (
                "group-writable-project",
                modes(&[("project", 0o770)]),
                Err("project"),
            ),
            (
                "group-writable-session-dir",
                modes(&[("session", 0o775)]),
                Err("session-dir"),
            ),
            (
                "group-writable-subagents",
                modes(&[("subagents", 0o775)]),
                Err("subagents"),
            ),
            (
                "group-writable-sessions",
                modes(&[("sessions", 0o770)]),
                Err("sessions"),
            ),
            (
                "group-writable-meta",
                modes(&[("meta", 0o664)]),
                Err("locator"),
            ),
            (
                "group-writable-record",
                modes(&[("record", 0o664)]),
                Err("session-record"),
            ),
            (
                "measured-live-modes",
                modes(&[
                    ("projects", 0o770),
                    ("session", 0o775),
                    ("subagents", 0o775),
                    ("record", 0o664),
                    ("meta", 0o664),
                ]),
                Err("session-record"),
            ),
            (
                "measured-live-modes-record-only-fixed",
                modes(&[
                    ("projects", 0o770),
                    ("session", 0o775),
                    ("subagents", 0o775),
                    ("meta", 0o664),
                ]),
                Err("projects"),
            ),
            (
                "sent-to-child",
                edit(|f| f.lead.push(send(AGENT, FOLLOW))),
                Err("locator"),
            ),
            (
                "send-before-turn-ignored",
                edit(|f| f.lead.insert(1, send(AGENT, FOLLOW))),
                Ok(OWN),
            ),
            (
                "depth-missing",
                edit(|f| {
                    f.meta.as_object_mut().unwrap().remove("spawnDepth");
                }),
                Err("locator"),
            ),
            (
                "depth-two",
                edit(|f| f.meta["spawnDepth"] = json!(2)),
                Err("locator"),
            ),
            (
                "depth-string",
                edit(|f| f.meta["spawnDepth"] = json!("1")),
                Err("locator"),
            ),
            (
                "lead-sidechain-absent",
                edit(|f| {
                    f.lead[5].as_object_mut().unwrap().remove("isSidechain");
                }),
                Err("lead-sidechain"),
            ),
            (
                "lead-sidechain-true",
                edit(|f| f.lead[6]["isSidechain"] = json!(true)),
                Err("lead-sidechain"),
            ),
            (
                "lead-turn-entry-sidechain-absent",
                edit(|f| {
                    f.lead[3].as_object_mut().unwrap().remove("isSidechain");
                }),
                Err("lead-sidechain"),
            ),
            (
                "pre-turn-sidechain-ignored",
                edit(|f| {
                    f.lead[1].as_object_mut().unwrap().remove("isSidechain");
                }),
                Ok(OWN),
            ),
            (
                "other-writable-subagents",
                modes(&[("subagents", 0o777)]),
                Err("subagents"),
            ),
            (
                "other-writable-meta",
                modes(&[("meta", 0o666)]),
                Err("locator"),
            ),
            (
                "other-writable-record",
                modes(&[("record", 0o666)]),
                Err("session-record"),
            ),
            (
                "oversize-record",
                edit(|f| f.record["padding"] = json!("x".repeat(16 * 1024))),
                Err("session-record"),
            ),
            (
                "oversize-meta",
                edit(|f| f.meta["padding"] = json!("x".repeat(16 * 1024))),
                Err("locator"),
            ),
            (
                "oversize-lead",
                fs_only(|f| {
                    let file = fs::OpenOptions::new()
                        .write(true)
                        .open(f.lead_path())
                        .unwrap();
                    file.set_len((1 << 30) + 1).unwrap();
                }),
                Err("lead-transcript"),
            ),
        ];
        for (label, mutate, expected) in cases {
            let mut fixture = Fixture::new(label);
            let locator = fixture.locator(AGENT);
            mutate(&mut fixture);
            let got = fixture.chat(&locator);
            // Compared without assert_eq so a failure prints only the stage.
            assert!(
                got.as_deref() == expected.as_deref(),
                "{label}: {:?}",
                got.err()
            );
            // Usage rides the same proof and rows: whatever refuses the chat withholds it.
            assert!(
                fixture.telemetry(&locator).is_ok() == got.is_ok(),
                "{label} usage"
            );
        }
    }

    fn human(source: Option<&str>, content: Value) -> Value {
        let mut entry = plain(content);
        entry["origin"] = json!({"kind": "human"});
        if let Some(source) = source {
            entry["promptSource"] = json!(source);
        }
        entry
    }

    /// A lead `user` entry with no origin, as slash commands and scheduled prompts are written.
    fn plain(content: Value) -> Value {
        json!({"type": "user", "sessionId": SESSION, "isSidechain": false,
               "message": {"role": "user", "content": content}})
    }

    fn with(mut entry: Value, key: &str, value: Value) -> Value {
        entry[key] = value;
        entry
    }

    #[test]
    fn a_newer_human_message_invalidates_the_turn() {
        let cases: Vec<(&str, Value)> = vec![
            ("queued", human(Some("queued"), json!("fixture redirect"))),
            (
                "typed-not-oms",
                human(Some("typed"), json!("fixture redirect")),
            ),
            (
                "suggestion-accepted",
                human(Some("suggestion_accepted"), json!("fixture redirect")),
            ),
            ("human-no-source", human(None, json!("fixture redirect"))),
            (
                "human-meta",
                with(human(Some("typed"), json!("x")), "isMeta", json!(true)),
            ),
            (
                "command-name-no-origin",
                plain(json!("<command-name>/clear</command-name>")),
            ),
            (
                "local-stdout-no-origin",
                plain(json!("<local-command-stdout>ok</local-command-stdout>")),
            ),
            ("sdk-no-origin", plain(json!("scheduled prompt"))),
            (
                "empty-origin-kind",
                with(plain(json!("x")), "origin", json!({"kind": ""})),
            ),
            (
                "origin-not-object",
                with(plain(json!("x")), "origin", json!("peer")),
            ),
            (
                "mixed-blocks",
                plain(
                    json!([{"type": "tool_result", "tool_use_id": "t", "content": "r"},
                             {"type": "text", "text": "x"}]),
                ),
            ),
            ("empty-blocks", plain(json!([]))),
            (
                "coordinator-origin",
                with(plain(json!("x")), "origin", json!({"kind": "coordinator"})),
            ),
            (
                "foo-origin",
                with(plain(json!("x")), "origin", json!({"kind": "foo"})),
            ),
            (
                "notification-meta",
                with(
                    with(
                        with(
                            plain(json!("<task-notification>\nx")),
                            "origin",
                            json!({"kind": "task-notification"}),
                        ),
                        "promptSource",
                        json!("system"),
                    ),
                    "isMeta",
                    json!(true),
                ),
            ),
            (
                "notification-no-header",
                with(
                    with(
                        plain(json!("hello <task-notification>\n")),
                        "origin",
                        json!({"kind": "task-notification"}),
                    ),
                    "promptSource",
                    json!("system"),
                ),
            ),
            (
                "peer-no-meta",
                with(
                    with(plain(json!("peer")), "origin", json!({"kind": "peer"})),
                    "promptSource",
                    json!("system"),
                ),
            ),
            (
                "origin-null",
                with(plain(json!("x")), "origin", json!(null)),
            ),
            (
                "system-source-no-meta",
                with(plain(json!("x")), "promptSource", json!("system")),
            ),
            (
                "tool-result-with-source",
                with(
                    plain(json!([{"type": "tool_result", "tool_use_id": "t", "content": "r"}])),
                    "promptSource",
                    json!("typed"),
                ),
            ),
        ];
        for (label, message) in cases {
            let mut fixture = Fixture::new(label);
            let locator = fixture.locator(AGENT);
            // Unchanged proof chain: only the lead's own file grew by a message.
            assert!(
                fixture.rows(None).is_ok_and(|rows| rows.len() == 1),
                "{label}"
            );
            fixture.lead.push(message);
            fixture.save();
            assert!(
                fixture.rows(None).err() == Some("newer-human"),
                "publish {label}"
            );
            assert!(
                fixture.rows(Some(TURN)).err() == Some("newer-human"),
                "read {label}"
            );
            assert!(
                fixture.chat(&locator).err() == Some("newer-human"),
                "chat {label}"
            );
        }
    }

    #[test]
    fn machine_entries_after_the_turn_keep_it_valid() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "tool-result-array",
                plain(json!([{"type": "tool_result", "tool_use_id": "t", "content": "r"}])),
            ),
            (
                "task-notification",
                with(
                    with(
                        plain(json!("<task-notification>\n<status>x</status>")),
                        "origin",
                        json!({"kind": "task-notification"}),
                    ),
                    "promptSource",
                    json!("system"),
                ),
            ),
            (
                "meta",
                with(plain(json!("<system-reminder/>")), "isMeta", json!(true)),
            ),
            (
                "meta-system",
                with(
                    with(plain(json!("<caveat/>")), "isMeta", json!(true)),
                    "promptSource",
                    json!("system"),
                ),
            ),
            (
                "compact-summary",
                with(plain(json!("summary")), "isCompactSummary", json!(true)),
            ),
            (
                "peer-meta",
                with(
                    with(
                        with(plain(json!("peer")), "isMeta", json!(true)),
                        "origin",
                        json!({"kind": "peer"}),
                    ),
                    "promptSource",
                    json!("system"),
                ),
            ),
            (
                "auto-continuation",
                with(
                    with(
                        with(plain(json!("x")), "isMeta", json!(true)),
                        "origin",
                        json!({"kind": "auto-continuation"}),
                    ),
                    "promptSource",
                    json!("system"),
                ),
            ),
        ];
        for (label, message) in cases {
            let mut fixture = Fixture::new(label);
            let locator = fixture.locator(AGENT);
            fixture.lead.push(message);
            fixture.save();
            assert!(
                fixture.rows(None).is_ok_and(|rows| rows.len() == 1),
                "{label}"
            );
            assert!(fixture.chat(&locator).as_deref() == Ok(OWN), "{label}");
        }
        // A person's message before the Turn does not matter: the Turn is the latest.
        let fixture = Fixture::new("human-before");
        assert!(
            fixture
                .lead
                .iter()
                .any(|entry| entry["promptSource"] == "typed")
        );
        assert!(fixture.rows(None).is_ok());
    }

    #[test]
    fn a_proven_child_of_an_oversize_lead_still_gets_a_row_and_a_chat() {
        use crate::orchestrator::{merge_claude_rows, painted_claude_children};
        let mut fixture = Fixture::new("long-lead");
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        fixture.lead[5] = assistant(json!([{"type": "tool_use", "name": "Agent", "id": TOOL,
            "input": {"prompt": PROMPT, "description": format!("{sample} {}", "d".repeat(200)),
                      "subagent_type": "general-purpose"}}]));
        let filler = padded(json!({"type": "progress", "sessionId": SESSION}), 65_536);
        let mut lead = vec![filler; LEAD_WINDOW as usize / 65_536 + 2];
        lead.append(&mut fixture.lead);
        fixture.lead = lead;
        fixture.save();
        assert!(fs::metadata(fixture.lead_path()).unwrap().len() > LEAD_WINDOW);

        // The real whole-file collector paints nothing for this lead.
        let old = crate::native_children::claude_in(&fixture.config, PID, &fixture.repo);
        assert!(old.unwrap_or_default().is_empty());

        let children = fixture.rows(None).unwrap();
        assert!(children.len() == 1 && children[0].title.chars().count() == 80);
        let locator = children[0].locator.clone();
        let mut rows = Vec::new();
        merge_claude_rows(&mut rows, &children);
        let id = crate::native_children::participant_id("claude", AGENT);
        assert!(rows.len() == 1 && rows[0]["participant"] == id);
        assert!(rows[0]["native_child"] == true && rows[0]["provider"] == "claude");
        assert!(rows[0]["state"] == "unknown" && children[0].state == "unknown");
        let title = rows[0]["title"].as_str().unwrap();
        assert!(
            !title.contains(&sample) && title.contains("oms-redacted"),
            "{title}"
        );
        let bound: Vec<_> = painted_claude_children(children, &rows).collect();
        assert!(bound.len() == 1 && bound[0].0 == id);
        assert!(fixture.chat(&locator).as_deref() == Ok(OWN));
    }

    #[test]
    fn small_lead_rows_come_once_from_the_collector_with_the_same_ids() {
        use crate::orchestrator::merge_claude_rows;
        let fixture = Fixture::new("dedup");
        let old = crate::native_children::claude_in(&fixture.config, PID, &fixture.repo).unwrap();
        let children = fixture.rows(None).unwrap();
        let id = crate::native_children::participant_id("claude", AGENT);
        let of = |rows: &[Value]| rows.iter().filter(|row| row["participant"] == id).count();
        assert!(of(&old) == 1 && children.len() == 1);

        // The collector's row wins and nothing is added beside it.
        let mut merged = old.clone();
        merge_claude_rows(&mut merged, &children);
        assert!(merged == old);

        // Without it the synthesized row has the same id and keys, exactly once.
        let mut missing: Vec<_> = old
            .iter()
            .filter(|row| row["participant"] != id)
            .cloned()
            .collect();
        let others = missing.len();
        merge_claude_rows(&mut missing, &children);
        merge_claude_rows(&mut missing, &children);
        assert!(missing.len() == others + 1 && of(&missing) == 1);
        let made = missing.iter().find(|row| row["participant"] == id).unwrap();
        let keys = |row: &Value| row.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let first = old.iter().find(|row| row["participant"] == id).unwrap();
        assert!(keys(made) == keys(first) && made["title"] == "fixture");
        assert!(made["native_answer"] == "" && first["native_answer"] != "");
    }

    fn notice(agent: &str, tool: &str, status: &str) -> Value {
        json!({"type": "user", "sessionId": SESSION, "isSidechain": false,
            "origin": {"kind": "task-notification"}, "promptSource": "system",
            "message": {"role": "user", "content": format!(
                "<task-notification>\n<task-id>{agent}</task-id>\n<tool-use-id>{tool}</tool-use-id>\n\
                 <output-file>/fixture/out</output-file>\n<status>{status}</status>\n\
                 <summary>s</summary>\n<result>r</result>\n</task-notification>")}})
    }

    fn raw_notice(content: &str) -> Value {
        let mut entry = notice(AGENT, TOOL, "completed");
        entry["message"]["content"] = json!(content);
        entry
    }

    #[test]
    fn a_synthesized_row_is_unknown_until_the_exact_completion_notice() {
        use crate::orchestrator::merge_claude_rows;
        let mut fixture = Fixture::new("state");
        let (spawn, end) = ("2026-11-22T01:00:00.000Z", "2026-11-22T01:05:00.000Z");
        fixture.lead[5] = stamped(fixture.lead[5].clone(), spawn);
        fixture.save();
        let locator = fixture.locator(AGENT);
        let clocks = |f: &Fixture| {
            let told = f.telemetry(&locator).unwrap();
            (told["spawned_at"].clone(), told["delivered_at"].clone())
        };
        let state = |f: &Fixture| {
            let children = f.rows(None).unwrap();
            assert!(children.len() == 1);
            let mut rows = Vec::new();
            merge_claude_rows(&mut rows, &children);
            assert_eq!(rows[0]["state"], children[0].state);
            children[0].state
        };
        assert_eq!(state(&fixture), "unknown");
        // No done notice: the completion is unknown, never guessed.
        assert_eq!(clocks(&fixture), (json!(spawn), Value::Null));
        let cases = [
            notice(AGENT, "toolu_other", "completed"),
            notice(OLD_AGENT, TOOL, "completed"),
            notice(AGENT, TOOL, "running"),
        ];
        for wrong in cases {
            fixture.lead.push(stamped(wrong, end));
            fixture.save();
            assert_eq!(state(&fixture), "unknown");
            assert_eq!(clocks(&fixture), (json!(spawn), Value::Null));
            fixture.lead.pop();
        }
        // Tags quoted inside another child's summary or result are not a header.
        let inner = |tail: &str| {
            raw_notice(&format!(
                "<task-notification>\n<task-id>{OLD_AGENT}</task-id>\n<tool-use-id>{OLD_TOOL}</tool-use-id>\n\
                 <status>completed</status>\n{tail}"
            ))
        };
        let quoted = format!(
            "<task-id>{AGENT}</task-id>\n<tool-use-id>{TOOL}</tool-use-id>\n<status>completed</status>\n"
        );
        for wrong in [
            inner(&format!("<summary>{quoted}</summary>\n")),
            inner(&format!("<result>{quoted}</result>\n")),
            raw_notice(&format!(
                "<task-notification>\n<tool-use-id>{TOOL}</tool-use-id>\n<task-id>{AGENT}</task-id>\n<status>completed</status>\n"
            )),
        ] {
            fixture.lead.push(wrong);
            fixture.save();
            assert_eq!(state(&fixture), "unknown");
            fixture.lead.pop();
        }
        // A notice from before the Turn is not evidence about this Turn.
        fixture.lead.insert(0, notice(AGENT, TOOL, "completed"));
        fixture.save();
        assert_eq!(state(&fixture), "unknown");
        fixture.lead.remove(0);
        fixture.lead.push(notice(AGENT, TOOL, "completed"));
        fixture.save();
        assert_eq!(state(&fixture), "done");
        // Done without a clock on the notice: done, completion still unknown.
        assert_eq!(clocks(&fixture), (json!(spawn), Value::Null));
        let done = fixture.lead.len() - 1;
        let exact = stamped(notice(AGENT, TOOL, "completed"), end);
        for (label, at, want) in [
            ("exact", end, json!(end)),
            ("before-spawn", "2026-11-22T00:59:59.999Z", Value::Null),
            ("not-rfc3339", "2026-11-22 01:05", Value::Null),
            ("garbage", "not a clock", Value::Null),
        ] {
            fixture.lead[done] = stamped(exact.clone(), at);
            fixture.save();
            assert_eq!(state(&fixture), "done", "{label}");
            assert_eq!(clocks(&fixture), (json!(spawn), want), "{label}");
        }
        fixture.lead[done] = exact.clone();
        // A second exact notice must name the same instant, whatever its spelling.
        for (other, want) in [
            ("2026-11-22T10:05:00+09:00", json!(end)),
            ("2026-11-22T01:06:00.000Z", Value::Null),
        ] {
            fixture.lead.push(stamped(exact.clone(), other));
            fixture.save();
            assert_eq!(clocks(&fixture), (json!(spawn), want), "{other}");
            fixture.lead.pop();
        }
        // An unparsable spawn is unknown; the exact notice after the call still counts.
        fixture.lead[5] = stamped(fixture.lead[5].clone(), "yesterday");
        fixture.save();
        assert_eq!(clocks(&fixture), (Value::Null, json!(end)));
        fixture.lead[5] = stamped(fixture.lead[5].clone(), spawn);
        fixture.lead[done] = notice(AGENT, TOOL, "completed");
        fixture.save();
        // The legacy header without an output file still counts.
        fixture.lead.pop();
        fixture.lead.push(raw_notice(&format!(
            "<task-notification>\n<task-id>{AGENT}</task-id>\n<tool-use-id>{TOOL}</tool-use-id>\n<status>completed</status>\n<summary>s</summary>\n"
        )));
        fixture.save();
        assert_eq!(state(&fixture), "done");
    }

    #[test]
    fn a_partial_final_line_fails_before_any_proof() {
        for (label, lead) in [("lead", true), ("child", false)] {
            let fixture = Fixture::new(&format!("partial-{label}"));
            let locator = fixture.locator(AGENT);
            let path = if lead {
                fixture.lead_path()
            } else {
                fixture.child_path(AGENT)
            };
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            write!(file, "{{\"type\":\"progress\",\"sessionId\":\"{SESSION}\"").unwrap();
            let stage = if lead {
                "lead-partial"
            } else {
                "child-partial"
            };
            if lead {
                assert_eq!(fixture.rows(None).err(), Some(stage), "{label} rows");
                assert_eq!(fixture.rows(Some(TURN)).err(), Some(stage), "{label} read");
            } else {
                assert!(fixture.rows(None).is_ok_and(|rows| rows.len() == 1));
            }
            assert_eq!(fixture.chat(&locator).err(), Some(stage), "{label} chat");
            // The line completing makes the file whole again.
            file.write_all(b"}\n").unwrap();
            assert!(fixture.rows(None).is_ok(), "{label} completed");
        }
    }

    #[test]
    fn child_user_arrays_other_than_tool_results_are_refused() {
        let image = json!([{"type": "image", "source": {"type": "base64"}}]);
        let text_block = json!([{"type": "text", "text": "x"}]);
        let mixed = json!([{"type": "tool_result", "tool_use_id": "t", "content": "r"},
                           {"type": "text", "text": "x"}]);
        for (label, content) in [
            ("text", text_block),
            ("image", image),
            ("empty", json!([])),
            ("mixed", mixed),
        ] {
            for (place, index) in [("before", 0), ("after-boundary", 2)] {
                let mut fixture = Fixture::new(&format!("array-{label}-{place}"));
                let locator = fixture.locator(AGENT);
                fixture
                    .child
                    .insert(index, child_user(AGENT, content.clone()));
                fixture.save();
                assert_eq!(
                    fixture.chat(&locator).err(),
                    Some("child-entry"),
                    "{label} {place}"
                );
            }
        }
    }

    #[test]
    fn a_large_turn_lists_and_reads_only_the_three_most_recently_answered() {
        const SPAWNED: usize = 40;
        let mut fixture = Fixture::new("roster");
        let id = |i: usize| format!("{:016x}", i + 1);
        let tool = |i: usize| format!("toolu_roster_{i}");
        let mut lead = vec![typed(&turn_prompt(TURN))];
        lead.extend((0..SPAWNED).map(|i| agent_call(&tool(i), &format!("prompt {i}"))));
        // Answers arrive in reverse, so the most recent ones are the first spawned.
        lead.extend((0..SPAWNED).rev().map(|i| spawned(&tool(i), &id(i))));
        fixture.lead = lead;
        fixture.save();
        for i in 0..SPAWNED {
            let (meta, body) = (fixture.meta_path(&id(i)), fixture.child_path(&id(i)));
            let reply = child_reply(&id(i), json!([{"type": "text", "text": "own reply"}]));
            let spawn = child_user(&id(i), json!(format!("prompt {i}")));
            fs::write(
                &meta,
                json!({"toolUseId": tool(i), "spawnDepth": 1}).to_string(),
            )
            .unwrap();
            fs::write(&body, lines(&[spawn, reply])).unwrap();
            mode(&meta, 0o644);
            mode(&body, 0o600);
        }
        META_READS.with(|n| n.set(0));
        let children = fixture.rows(Some(TURN)).unwrap();
        assert_eq!(META_READS.with(|n| n.get()), 3);
        let agents: Vec<_> = children.iter().map(|c| c.locator.agent.clone()).collect();
        assert_eq!(agents, [id(0), id(1), id(2)]);
        assert!(children.iter().all(|c| c.state == "unknown"));
        // Usage is read for the USAGE_TAILS newest spawns only; the oldest keeps its row.
        let board = |tails| {
            current(&fixture.lead(), &fixture.repo, tails, || {})
                .unwrap()
                .1
        };
        let read = |tails| {
            board(tails)
                .iter()
                .map(|c| c.usage.is_some())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            (USAGE_TAILS, read(USAGE_TAILS)),
            (2, vec![false, true, true])
        );
        assert_eq!(read(0), [false; 3]);

        // A failed proof is not refilled from an older child.
        fs::remove_file(fixture.meta_path(&id(1))).unwrap();
        META_READS.with(|n| n.set(0));
        let agents: Vec<_> = fixture
            .rows(Some(TURN))
            .unwrap()
            .iter()
            .map(|c| c.locator.agent.clone())
            .collect();
        assert_eq!(
            (META_READS.with(|n| n.get()), agents),
            (3, vec![id(0), id(2)])
        );

        // Only the listed children can be opened.
        let kept = fixture.locator(&id(0));
        assert_eq!(fixture.chat(&kept).as_deref(), Ok("own reply"));
        let mut older = kept.clone();
        (older.agent, older.tool_use) = (id(SPAWNED - 1), tool(SPAWNED - 1));
        assert_eq!(fixture.chat(&older).err(), Some("locator"));
    }

    #[test]
    fn children_are_only_this_turns_direct_spawns() {
        let mut fixture = Fixture::new("listing");
        let found = fixture.children(TURN).unwrap();
        // The earlier Turn's child, even messaged this Turn, is never listed.
        assert!(found.len() == 1);
        let own = &found[0];
        assert!(own.agent == AGENT && own.tool_use == TOOL);
        // The locator keeps a digest of the prompt, not the prompt.
        assert!(own.boundary == digest(PROMPT.as_bytes()) && own.boundary != PROMPT);
        let lead = fs::metadata(fixture.lead_path()).unwrap();
        let child = fs::metadata(fixture.child_path(AGENT)).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert!(own.pid == PID && own.birth == BIRTH && own.session == SESSION && own.turn == TURN);
        assert!(own.lead == (lead.dev(), lead.ino()) && own.child == (child.dev(), child.ino()));
        assert!(fixture.children(STALE).err() == Some("stale-turn"));

        // One Agent call answered twice binds nothing.
        fixture.lead.push(spawned(TOOL, OLD_AGENT));
        fixture.save();
        assert!(fixture.children(TURN).unwrap().is_empty());
        fixture.lead.retain(|entry| {
            managed_turn(entry) != Some(STALE) && managed_turn(entry) != Some(TURN)
        });
        fixture.save();
        assert!(fixture.children(TURN).err() == Some("no-turn"));
    }

    #[test]
    fn current_scan_names_the_last_managed_turn() {
        let mut fixture = Fixture::new("current");
        let current = |f: &Fixture| {
            f.rows(None).and_then(|rows| {
                let turn = scan(&f.lead(), &f.repo, None)?.turn;
                Ok((
                    turn,
                    rows.into_iter().map(|c| c.locator).collect::<Vec<_>>(),
                ))
            })
        };
        let (turn, children) = current(&fixture).unwrap();
        assert!(turn == TURN && children.len() == 1 && children[0].turn == TURN);
        assert!(children == fixture.children(TURN).unwrap());

        // A newer Turn replaces the current one: its children start empty and
        // the older Turn's locator no longer reads.
        let stale = fixture.locator(AGENT);
        fixture.lead.push(typed(&turn_prompt(NEWER)));
        fixture.save();
        let (turn, children) = current(&fixture).unwrap();
        assert!(turn == NEWER && children.is_empty());
        assert!(fixture.chat(&stale).err() == Some("stale-turn"));
        assert!(fixture.telemetry(&stale).err() == Some("locator"));

        fixture.lead.retain(|entry| managed_turn(entry).is_none());
        fixture.save();
        assert!(current(&fixture).err() == Some("no-turn"));
    }

    #[test]
    fn child_chat_redacts_secret_shaped_text() {
        let mut fixture = Fixture::new("redact");
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        fixture.child.push(child_reply(
            AGENT,
            json!([{"type": "text", "text": format!("leak {sample} end")}]),
        ));
        fixture.save();
        let locator = fixture.locator(AGENT);
        let shown = fixture.chat(&locator).unwrap();
        assert!(shown.contains("[oms-redacted:github-token]") && !shown.contains(&sample));
    }

    #[test]
    fn child_usage_is_the_newest_response_of_the_child_only_or_unknown() {
        const T1: &str = "2026-11-22T01:01:00.000Z";
        const T2: &str = "2026-11-22T01:02:00.000Z";
        const T3: &str = "2026-11-22T01:03:00.000Z";
        let mut fixture = Fixture::new("telemetry");
        let locator = fixture.locator(AGENT);
        let latest = |f: &Fixture| f.telemetry(&locator).unwrap()["response"].clone();
        let response = |id: &str, at: &str, n: [u64; 4]| {
            json!({"message_id": id, "timestamp": at, "input_tokens": n[0],
                   "output_tokens": n[1], "cache_creation_input_tokens": n[2],
                   "cache_read_input_tokens": n[3], "total_tokens": n.iter().sum::<u64>()})
        };
        // Rows without usage, and nothing from the lead: every field is unknown.
        assert_eq!(
            fixture.telemetry(&locator).unwrap(),
            json!({"scope": "latest_child_response", "response": null,
                   "spawned_at": null, "delivered_at": null})
        );
        // A billed row before the spawn prompt belongs to no response of this spawn.
        fixture.child.insert(
            0,
            billed(child_reply(AGENT, json!([])), "msg_z", T3, [9, 9, 9, 9]),
        );
        fixture.save();
        assert_eq!(latest(&fixture), Value::Null);
        fixture.child.remove(0);

        fixture.child[1] = billed(fixture.child[1].clone(), "msg_a", T1, [10, 2, 3, 4]);
        fixture.child[3] = billed(fixture.child[3].clone(), "msg_b", T2, [11, 5, 6, 7]);
        fixture.save();
        let newest = response("msg_b", T2, [11, 5, 6, 7]);
        assert_eq!(latest(&fixture), newest);
        assert!(fixture.chat(&locator).as_deref() == Ok(OWN));

        // A streaming duplicate of the newest response counts once, timed by its last row.
        let tool = json!([{"type": "tool_use", "name": "Read", "id": "toolu_more", "input": {}}]);
        let repeat = |n: [i64; 4]| billed(child_reply(AGENT, tool.clone()), "msg_b", T3, n);
        fixture.child.push(repeat([11, 5, 6, 7]));
        fixture.save();
        assert_eq!(latest(&fixture), response("msg_b", T3, [11, 5, 6, 7]));
        assert!(fixture.chat(&locator).as_deref() == Ok(OWN));

        // The same id with other counters is conflicting: unknown, not the older msg_a.
        *fixture.child.last_mut().unwrap() = repeat([11, 6, 6, 7]);
        fixture.save();
        assert_eq!(latest(&fixture), Value::Null);
        fixture.child.pop();

        // A newest row that does not prove itself makes the latest unknown,
        // never the older valid msg_b.
        let reply = || child_reply(AGENT, tool.clone());
        let without = |path: &[&str]| {
            let mut row = billed(reply(), "msg_c", T3, [1, 1, 1, 1]);
            let (key, parents) = path.split_last().unwrap();
            let parent = parents.iter().fold(&mut row, |row, part| &mut row[*part]);
            parent.as_object_mut().unwrap().remove(*key);
            row
        };
        let field = |key: &str, value: Value| {
            let mut row = billed(reply(), "msg_c", T3, [1, 1, 1, 1]);
            row["message"]["usage"][key] = value;
            row
        };
        let unknown = [
            (
                "not-rfc3339",
                billed(reply(), "msg_c", "2026-11-22 01:03:00", [1, 1, 1, 1]),
            ),
            ("unparsable", billed(reply(), "msg_c", "soon", [1, 1, 1, 1])),
            ("unclocked", without(&["timestamp"])),
            (
                "missing",
                without(&["message", "usage", "cache_read_input_tokens"]),
            ),
            ("no-usage", without(&["message", "usage"])),
            ("negative", billed(reply(), "msg_c", T3, [1, -1, 1, 1])),
            ("float", field("input_tokens", json!(1.5))),
            ("string", field("output_tokens", json!("1"))),
            ("null", field("cache_creation_input_tokens", Value::Null)),
            ("no-id", without(&["message", "id"])),
            ("odd-id", billed(reply(), "msg c/../x", T3, [1, 1, 1, 1])),
        ];
        // Every case runs before the verdict, so a fallback names all it hit.
        let mut fell_back = Vec::new();
        for (label, row) in unknown {
            fixture.child.push(row);
            fixture.save();
            if latest(&fixture) != Value::Null {
                fell_back.push(label);
            }
            // A later valid response is the latest again.
            fixture
                .child
                .push(billed(reply(), "msg_d", T3, [2, 2, 2, 2]));
            fixture.save();
            assert_eq!(
                latest(&fixture),
                response("msg_d", T3, [2, 2, 2, 2]),
                "{label}"
            );
            fixture.child.truncate(fixture.child.len() - 2);
        }
        assert!(fell_back.is_empty(), "fell back: {fell_back:?}");
        // Rows other than the child's assistant replies are no response at all.
        let others = [
            (
                "user-row",
                billed(
                    child_user(
                        AGENT,
                        json!([{"type": "tool_result", "tool_use_id": "toolu_more",
                                "content": "r"}]),
                    ),
                    "msg_c",
                    T3,
                    [1, 1, 1, 1],
                ),
            ),
            (
                "progress-row",
                billed(
                    json!({"type": "progress", "sessionId": SESSION, "message": {}}),
                    "msg_c",
                    T3,
                    [1, 1, 1, 1],
                ),
            ),
        ];
        for (label, row) in others {
            fixture.child.push(row);
            fixture.save();
            assert_eq!(latest(&fixture), newest, "{label}");
            fixture.child.pop();
        }
        // A later message the child received ends the segment: neither a valid
        // nor a malformed response to it touches the latest.
        fixture.child.push(child_user(AGENT, json!(FOLLOW)));
        for row in [
            billed(reply(), "msg_f", T3, [3, 3, 3, 3]),
            billed(reply(), "msg_g", "soon", [3, 3, 3, 3]),
        ] {
            fixture.child.push(row);
            fixture.save();
            assert_eq!(latest(&fixture), newest);
            assert!(fixture.chat(&locator).as_deref() == Ok(OWN));
        }
        fixture.child.truncate(fixture.child.len() - 3);
        // One malformed row spoils its whole response, before or after a valid twin.
        let twin = billed(reply(), "msg_c", T3, [1, 1, 1, 1]);
        let spoiled = billed(reply(), "msg_c", "soon", [1, 1, 1, 1]);
        for pair in [[twin.clone(), spoiled.clone()], [spoiled, twin]] {
            fixture.child.extend(pair);
            fixture.save();
            assert_eq!(latest(&fixture), Value::Null);
            fixture.child.truncate(fixture.child.len() - 2);
        }
        // The total is the checked sum of the four counters; overflow is unknown.
        for (input, want) in [(u64::MAX - 3, json!(u64::MAX)), (u64::MAX - 2, Value::Null)] {
            let mut row = billed(reply(), "msg_e", T3, [1, 1, 1, 1]);
            row["message"]["usage"]["input_tokens"] = json!(input);
            fixture.child.push(row);
            fixture.save();
            assert_eq!(latest(&fixture)["total_tokens"], want, "{input}");
            fixture.child.pop();
        }
    }

    #[test]
    fn messaged_child_is_suppressed_even_beside_stale_wrappers() {
        let mut fixture = Fixture::new("suppressed");
        let locator = fixture.locator(AGENT);
        // The child's file also holds an older wrapper and an identical older prompt.
        fixture.child.insert(0, wrapped(AGENT, FOLLOW));
        fixture.child.insert(0, child_user(AGENT, json!(PROMPT)));
        // No delivery was observed: the lead's SendMessage alone suppresses it.
        fixture.lead.push(send(AGENT, FOLLOW));
        fixture.save();
        assert!(fixture.children(TURN).unwrap().is_empty());
        assert!(fixture.chat(&locator).err() == Some("locator"));
    }

    #[test]
    fn an_older_wrapper_ends_the_segment_and_is_never_a_boundary() {
        let mut fixture = Fixture::new("wrapper");
        let locator = fixture.locator(AGENT);
        fixture.child.insert(
            0,
            child_reply(AGENT, json!([{"type": "text", "text": "stale"}])),
        );
        fixture.child.insert(0, wrapped(AGENT, PROMPT));
        fixture.save();
        assert!(fixture.chat(&locator).as_deref() == Ok(OWN));
        // A wrapper after the prompt ends the segment: later replies are not shown.
        fixture.child.insert(4, wrapped(AGENT, FOLLOW));
        fixture.child.insert(
            5,
            child_reply(AGENT, json!([{"type": "text", "text": "later"}])),
        );
        fixture.save();
        assert!(fixture.chat(&locator).as_deref() == Ok("first reply"));
        // Usage ends with the segment: a valid response past it answers the later
        // message, so the latest is the segment's own newest response or unknown.
        let at = "2026-11-22T01:00:00Z";
        let response = |f: &Fixture| f.telemetry(&locator).unwrap()["response"].clone();
        let last = fixture.child.pop().unwrap();
        fixture
            .child
            .push(billed(last, "msg_later", at, [1, 2, 3, 4]));
        fixture.save();
        assert_eq!(response(&fixture), Value::Null);
        let first = fixture.child[3].clone();
        fixture.child[3] = billed(first.clone(), "msg_first", at, [5, 6, 7, 8]);
        fixture.save();
        assert!(response(&fixture)["message_id"] == "msg_first");
        // A malformed response past the segment does not spoil the segment's latest.
        fixture
            .child
            .push(billed(first.clone(), "msg_bad", "soon", [1, 1, 1, 1]));
        fixture.save();
        assert!(response(&fixture)["message_id"] == "msg_first");
        assert!(fixture.chat(&locator).as_deref() == Ok("first reply"));
        // An invalid newest row within the segment stays unknown, never the later one.
        fixture.child[3] = billed(first, "msg_first", "soon", [5, 6, 7, 8]);
        fixture.save();
        assert_eq!(response(&fixture), Value::Null);
        assert!(fixture.chat(&locator).as_deref() == Ok("first reply"));
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let (from, to) = (
                entry.as_ref().unwrap().path(),
                to.join(entry.unwrap().file_name()),
            );
            if from.is_dir() {
                copy_tree(&from, &to);
            } else {
                fs::copy(&from, &to).unwrap();
            }
        }
        mode(
            to,
            fs::metadata(from).unwrap().permissions().mode() & 0o7777,
        );
    }

    #[test]
    fn renamed_ancestors_and_files_are_refused() {
        type Swap = fn(&Fixture);
        let cases: [(Swap, &str); 3] = [
            (
                |f| {
                    let projects = f.config.join("projects");
                    let away = f.base.join("away");
                    fs::rename(&projects, &away).unwrap();
                    copy_tree(&away, &projects);
                },
                "ancestor",
            ),
            (
                |f| {
                    let away = f.base.join("away-subagents");
                    fs::rename(f.subagents(), &away).unwrap();
                    copy_tree(&away, &f.subagents());
                },
                "subagents",
            ),
            (
                |f| {
                    let copy = f.base.join("meta-copy");
                    fs::copy(f.meta_path(AGENT), &copy).unwrap();
                    fs::rename(&copy, f.meta_path(AGENT)).unwrap();
                },
                "meta-file",
            ),
        ];
        for (swap, label) in cases {
            let fixture = Fixture::new(label);
            let locator = fixture.locator(AGENT);
            let got = chat(&fixture.lead(), &fixture.repo, TURN, &locator, || {
                swap(&fixture)
            });
            assert!(got.err() == Some("path-changed"), "chat {label}");
            let fixture = Fixture::new(&format!("{label}-scan"));
            let scan = scan(&fixture.lead(), &fixture.repo, Some(TURN)).unwrap();
            swap(&fixture);
            assert!(scan.confirm().err() == Some("path-changed"), "scan {label}");
        }
    }

    #[test]
    fn changes_between_read_and_recheck_are_refused() {
        let fixture = Fixture::new("rewrite");
        let locator = fixture.locator(AGENT);
        let reply = child_reply(AGENT, json!([{"type": "text", "text": "late"}]));
        let lead_reply = assistant(json!([{"type": "text", "text": "late"}]));
        let switched = json!({"pid": PID, "procStart": "987654", "sessionId": OTHER_SESSION,
                              "cwd": fixture.repo.to_str().unwrap()});
        let cases: [(&dyn Fn(), &str); 3] = [
            (
                &|| append(&fixture.child_path(AGENT), &reply),
                "child-changed",
            ),
            (
                &|| append(&fixture.lead_path(), &lead_reply),
                "lead-changed",
            ),
            (
                &|| fs::write(fixture.record_path(), switched.to_string()).unwrap(),
                "session-switched",
            ),
        ];
        for (between, stage) in cases {
            let got = chat(&fixture.lead(), &fixture.repo, TURN, &locator, between);
            assert!(
                got.as_ref().err() == Some(&stage),
                "{stage}: {:?}",
                got.err()
            );
        }
        // After the Board's usage reads, a lead change or session switch withholds
        // usage only: the rows were proven before those reads.
        let fixture = Fixture::new("rewrite-board");
        let board = |between: &dyn Fn()| {
            current(&fixture.lead(), &fixture.repo, USAGE_TAILS, between)
                .unwrap()
                .1
        };
        assert!(board(&|| {})[0].usage.is_some());
        let cases: [&dyn Fn(); 2] = [&|| append(&fixture.lead_path(), &lead_reply), &|| {
            fs::write(fixture.record_path(), switched.to_string()).unwrap()
        }];
        for (case, between) in cases.into_iter().enumerate() {
            let children = board(between);
            assert!(children.len() == 1 && children[0].usage.is_none(), "{case}");
        }
        // The re-walk also proves type, owner/mode and link count of what it reached.
        let fixture = Fixture::new("rewalk-attributes");
        let locator = fixture.locator(AGENT);
        let twin = fixture.base.join("twin");
        let cases: [(&str, &dyn Fn()); 3] = [
            ("file-mode", &|| mode(&fixture.meta_path(AGENT), 0o664)),
            ("file-link", &|| {
                fs::hard_link(fixture.meta_path(AGENT), &twin).unwrap()
            }),
            ("dir-mode", &|| {
                mode(&fixture.config.join("projects"), 0o770)
            }),
        ];
        for (label, between) in cases {
            let got = chat(&fixture.lead(), &fixture.repo, TURN, &locator, between);
            assert!(
                got.as_ref().err() == Some(&"path-changed"),
                "{label}: {:?}",
                got.err()
            );
            fixture.save();
            let _ = fs::remove_file(&twin);
        }
    }

    #[test]
    fn tail_windows_bound_both_transcripts() {
        let mut fixture = Fixture::new("windows");
        let spawn = "2026-11-22T01:00:00Z";
        fixture.lead[5] = stamped(fixture.lead[5].clone(), spawn);
        fixture.child[3] = billed(fixture.child[3].clone(), "msg_w", spawn, [1, 2, 3, 4]);
        let filler = |n: usize| padded(json!({"type": "progress", "sessionId": SESSION}), n);
        // Unreadable lines before the Turn and a mid-record window start are tolerated.
        let mut lead = vec![padded(assistant(json!([])), LINE_LIMIT)];
        lead.extend((0..(LEAD_WINDOW as usize / 65_536 + 2)).map(|_| filler(65_536)));
        lead.append(&mut fixture.lead);
        fixture.lead = lead;
        fixture.save();
        assert!(fs::metadata(fixture.lead_path()).unwrap().len() > LEAD_WINDOW);
        let locator = fixture.locator(AGENT);
        assert!(fixture.chat(&locator).as_deref() == Ok(OWN));
        // Usage reads only the same windows: the spawn row lies inside the lead's.
        let told = fixture.telemetry(&locator).unwrap();
        assert!(told["spawned_at"] == spawn && told["response"]["message_id"] == "msg_w");
        // A boundary that left the child window is not guessed.
        let mut child = fixture.child.clone();
        child.extend((0..(CHILD_WINDOW as usize / 65_536 + 2)).map(|_| filler(65_536)));
        fixture.child = child;
        fixture.save();
        assert!(fixture.chat(&locator).err() == Some("no-boundary"));
        // The child keeps its row; only its usage is withheld.
        assert!(fixture.telemetry(&locator).err() == Some("withheld"));
    }

    #[test]
    fn own_process_birth_gates_the_session_record() {
        let mut fixture = Fixture::new("own-pid");
        let pid = std::process::id();
        let birth = oms_panel_core::terminals::process(pid).unwrap().birth;
        fixture.record["pid"] = json!(pid);
        fixture.record["procStart"] = json!(start_time(&birth).unwrap());
        fixture.save();
        fs::rename(
            fixture.config.join(format!("sessions/{PID}.json")),
            fixture.config.join(format!("sessions/{pid}.json")),
        )
        .unwrap();
        let read = |turn: &str| {
            proven(pid, |birth| {
                Ok(scan(
                    &Lead::new(pid, birth, &fixture.config),
                    &fixture.repo,
                    Some(turn),
                )?
                .children
                .len())
            })
        };
        assert!(read(TURN) == Ok(1));
        assert!(read(STALE).err() == Some("stale-turn"));
        let forged = json!({"pid": pid, "procStart": "1", "sessionId": SESSION,
                            "cwd": fixture.repo.to_str().unwrap()});
        fs::write(
            fixture.config.join(format!("sessions/{pid}.json")),
            forged.to_string(),
        )
        .unwrap();
        assert!(read(TURN).err() == Some("session-record"));

        // The entry points never read the panel's own roots for an unprovable pid.
        let locator = ClaudeChildLocator {
            pid: u32::MAX,
            birth: birth.clone(),
            session: SESSION.into(),
            turn: TURN.into(),
            agent: AGENT.into(),
            tool_use: TOOL.into(),
            boundary: digest(PROMPT.as_bytes()),
            lead: (0, 0),
            child: (0, 0),
        };
        assert!(claude_current_children(u32::MAX, &fixture.repo).is_none());
        assert!(claude_child_chat(&fixture.repo, TURN, &locator).is_none());
        assert!(claude_board_children(u32::MAX, &fixture.repo).is_none());
    }

    #[test]
    fn managed_turn_accepts_only_the_typed_task_shape() {
        let prompt = turn_prompt(TURN);
        assert!(managed_turn(&typed(&prompt)) == Some(TURN));
        assert!(
            managed_turn(&typed(&format!("<pasted_content id=\"ab12\">{prompt}"))) == Some(TURN)
        );
        for content in [
            format!("<pasted_content id=\"zz\">{prompt}"),
            format!("<pasted_content id=\"ab12\">x{prompt}"),
            format!(" {prompt}"),
            prompt.replace(TURN, "1234567890ABCDEF1234"),
        ] {
            assert!(managed_turn(&typed(&content)).is_none());
        }
        let mut queued = typed(&prompt);
        queued["promptSource"] = json!("queued");
        let mut agent = typed(&prompt);
        agent["origin"]["kind"] = json!("agent");
        assert!(managed_turn(&queued).is_none() && managed_turn(&agent).is_none());
    }
}
