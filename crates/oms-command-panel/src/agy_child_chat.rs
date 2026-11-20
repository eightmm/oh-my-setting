//! Proven direct child chat for Antigravity (`agy`) lead panes.
//!
//! Proof chain: Phase A (`agy_lead_custody`) proves the live `agy` process, its
//! canonical repository CWD, private `.gemini/antigravity-cli` tree, and unique
//! held `presence/<lead_id>.lock` WRITE flock. Phase C opens only descriptor-
//! anchored `brain/<uuid>/.system_generated/logs/transcript_full.jsonl` files
//! (`O_NOFOLLOW`, `0o022 == 0`, `st_nlink == 1`), validates worktrees strictly
//! via `AgyStrictWorkspaces`, delegates pure record attribution to Phase B
//! (`agy_child_records`), runs `post_fence` while holding descriptors open, and
//! re-walks every traversed path and custody check after reading.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

/// Selected painted AGY row bound to a live lead pane process and Turn. No `Debug` derive.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgySelectedChild {
    pub(crate) pid: u32,
    pub(crate) birth: String,
    pub(crate) turn: String,
    pub(crate) participant: String,
}

/// Opaque identity binding a proven AGY child to its live lead process, lock,
/// transcripts, and Phase B Turn boundary. No `Debug` derive: contains private facts.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgyChildChatLocator {
    pub(crate) pid: u32,
    pub(crate) birth: String,
    pub(crate) app_root: PathBuf,
    pub(crate) record: crate::agy_child_records::AgyChildLocator,
    pub(crate) lock: (rustix::fs::Dev, u64),
    pub(crate) lead: (rustix::fs::Dev, u64),
    pub(crate) lead_min_size: u64,
    pub(crate) child: (rustix::fs::Dev, u64),
    pub(crate) child_min_size: u64,
}

/// Result of verifying and opening an explicit selected AGY child row. No `Debug` derive.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AgyChatOpen {
    #[cfg(target_os = "linux")]
    OwnChat {
        history: String,
        locator: AgyChildChatLocator,
    },
    Unbound,
    Unavailable,
}

fn unsupported_open_selected(post_fence: impl FnOnce() -> bool) -> AgyChatOpen {
    if post_fence() {
        AgyChatOpen::Unbound
    } else {
        AgyChatOpen::Unavailable
    }
}

fn unsupported_locator_current_fenced(post_fence: impl FnOnce() -> bool) -> bool {
    let _ = post_fence;
    false
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn agy_open_selected_child_fenced(
    _repo: &Path,
    _selected: &AgySelectedChild,
    post_fence: impl FnOnce() -> bool,
) -> AgyChatOpen {
    unsupported_open_selected(post_fence)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn agy_locator_still_current_fenced(
    _repo: &Path,
    _expected: &AgyChildChatLocator,
    post_fence: impl FnOnce() -> bool,
) -> bool {
    unsupported_locator_current_fenced(post_fence)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) use linux::{
    agy_child_chat_fenced, agy_child_read_counts, agy_current_children, agy_locator_still_current,
    legacy_agy_read_index, reset_all_agy_counters, reset_test_counters, test_counters,
    total_agy_child_reads,
};
#[cfg(target_os = "linux")]
pub(crate) use linux::{agy_locator_still_current_fenced, agy_open_selected_child_fenced};

#[cfg(target_os = "linux")]
mod linux {
    use super::{AgyChatOpen, AgyChildChatLocator, AgySelectedChild};
    use crate::agy_child_records::{
        CHILD_BYTE_BUDGET, LEAD_BYTE_BUDGET, extract_child_chat,
        extract_selected_child_chat_and_setup, format_selected_child_reader, scan_lead_turn,
    };
    use crate::agy_lead_custody::{AgyLeadCustody, agy_lead_custody};
    use crate::claude_lead_usage::linux::{DIR, FILE, open_dir, unchanged};
    use crate::native_children::{AgyStrictWorkspaces, agy_dir_strict, participant_id};
    use oms_panel_core::native::reference::process::Owner;
    use oms_panel_core::terminals;
    use rustix::fs::{self, FileType, Mode, Stat};
    use rustix::process::{geteuid, getuid};
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::OwnedFd;
    use std::path::Path;
    use std::rc::Rc;

    const LOCK_FILE_LIMIT: u64 = 4096;
    const TRANSCRIPT_LIMIT: u64 = 1 << 30;

    #[cfg(test)]
    thread_local!(pub(super) static CUSTODY_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
    #[cfg(test)]
    thread_local!(pub(super) static PARENT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
    #[cfg(test)]
    thread_local!(pub(super) static CHILD_BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

    #[cfg(test)]
    pub(crate) fn reset_test_counters() {
        CUSTODY_CHECKS.with(|n| n.set(0));
        PARENT_SCANS.with(|n| n.set(0));
        CHILD_BODY_READS.with(|n| n.set(0));
    }

    #[cfg(test)]
    pub(crate) fn test_counters() -> (usize, usize, usize) {
        (
            CUSTODY_CHECKS.with(|n| n.get()),
            PARENT_SCANS.with(|n| n.get()),
            CHILD_BODY_READS.with(|n| n.get()),
        )
    }

    #[cfg(test)]
    pub(crate) fn reset_all_agy_counters() {
        crate::native_children::reset_agy_legacy_reads();
        reset_test_counters();
    }

    #[cfg(test)]
    pub(crate) fn agy_child_read_counts() -> (usize, usize) {
        (
            crate::native_children::agy_legacy_child_reads(),
            CHILD_BODY_READS.with(|n| n.get()),
        )
    }

    #[cfg(test)]
    pub(crate) fn legacy_agy_read_index() -> (usize, usize, usize, usize) {
        crate::native_children::legacy_agy_read_index()
    }

    #[cfg(test)]
    pub(crate) fn total_agy_child_reads() -> usize {
        let (legacy, strict) = agy_child_read_counts();
        legacy + strict
    }

    fn checked_lead_custody(pid: u32, birth: &str, repo: &Path) -> Option<AgyLeadCustody> {
        #[cfg(test)]
        CUSTODY_CHECKS.with(|n| n.set(n.get() + 1));
        agy_lead_custody(pid, birth, repo)
    }

    pub(super) type Stage<T> = Result<T, &'static str>;

    fn gate(ok: bool, stage: &'static str) -> Stage<()> {
        if ok { Ok(()) } else { Err(stage) }
    }

    fn is_turn(s: &str) -> bool {
        s.len() == 20
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    pub(crate) fn agy_current_children(
        pid: u32,
        birth: &str,
        repo: &Path,
        expected_turn: Option<&str>,
        painted: &BTreeSet<String>,
    ) -> Option<(String, Vec<AgyChildChatLocator>)> {
        agy_current_children_stage(pid, birth, repo, expected_turn, painted).ok()
    }

    pub(crate) fn agy_child_chat_fenced(
        repo: &Path,
        turn: &str,
        expected: &AgyChildChatLocator,
        post_fence: impl FnOnce() -> bool,
    ) -> Option<String> {
        agy_child_chat_fenced_stage(repo, turn, expected, post_fence).ok()
    }

    pub(crate) fn agy_open_selected_child_fenced(
        repo: &Path,
        selected: &AgySelectedChild,
        post_fence: impl FnOnce() -> bool,
    ) -> AgyChatOpen {
        let Ok((uid, owner)) = (|| -> Stage<(u32, Owner)> {
            gate(!selected.participant.is_empty(), "locator")?;
            gate(is_turn(&selected.turn), "locator")?;
            verify_process(selected.pid, &selected.birth)
        })() else {
            return AgyChatOpen::Unavailable;
        };

        let Some(custody) = checked_lead_custody(selected.pid, &selected.birth, repo) else {
            return if post_fence() && terminals::process(selected.pid).is_ok_and(|now| now == owner)
            {
                AgyChatOpen::Unbound
            } else {
                AgyChatOpen::Unavailable
            };
        };

        let Ok(app_dir) = open_presence_app(&custody, uid, custody.lock) else {
            return AgyChatOpen::Unavailable;
        };

        let brain_dir = match app_dir.dir("brain", uid, "brain-dir") {
            Ok(dir) => dir,
            Err(_) => {
                // Phase A custody does not inspect `brain/`; a genuine uid-owned directory with
                // relaxed `0o022 != 0` falls back to Unbound once post-fence & custody recheck pass.
                let relaxed_brain = fs::openat(&app_dir.fd, "brain", DIR, Mode::empty())
                    .ok()
                    .and_then(|fd| fs::fstat(&fd).ok())
                    .is_some_and(|st| {
                        FileType::from_raw_mode(st.st_mode) == FileType::Directory
                            && st.st_uid == uid
                            && st.st_mode & 0o022 != 0
                    });
                if relaxed_brain
                    && post_fence()
                    && same_names(&custody.app_root, uid, &app_dir.trail.borrow()).is_ok()
                    && checked_lead_custody(selected.pid, &selected.birth, repo).as_ref()
                        == Some(&custody)
                    && terminals::process(selected.pid).is_ok_and(|now| now == owner)
                {
                    return AgyChatOpen::Unbound;
                }
                return AgyChatOpen::Unavailable;
            }
        };

        let Ok((lead_file, lead_stat)) = open_transcript(
            &brain_dir,
            &custody.conversation_id,
            TRANSCRIPT_LIMIT,
            uid,
            "lead-transcript",
        ) else {
            return AgyChatOpen::Unavailable;
        };
        let Ok((parent_jsonl, parent_recent)) = read_parent_tail(&lead_file, &lead_stat) else {
            return AgyChatOpen::Unavailable;
        };

        #[cfg(test)]
        PARENT_SCANS.with(|n| n.set(n.get() + 1));

        let Some(mut strict_ws) = AgyStrictWorkspaces::new(&custody.repo) else {
            let Some(_) = scan_lead_turn(
                &parent_jsonl,
                parent_recent,
                &custody.conversation_id,
                Some(&selected.turn),
                |_| false,
            ) else {
                return AgyChatOpen::Unavailable;
            };
            if post_fence()
                && confirm_open_custody(
                    repo,
                    selected.pid,
                    &selected.birth,
                    &owner,
                    uid,
                    &custody,
                    &brain_dir.trail.borrow(),
                    (&lead_file, &lead_stat),
                    None,
                    None,
                )
                .is_ok()
            {
                return AgyChatOpen::Unbound;
            }
            return AgyChatOpen::Unavailable;
        };

        let Some(scan) = scan_lead_turn(
            &parent_jsonl,
            parent_recent,
            &custody.conversation_id,
            Some(&selected.turn),
            |uri| strict_ws.accepts(uri),
        ) else {
            return AgyChatOpen::Unavailable;
        };

        let mut matches: Vec<_> = scan
            .children
            .into_iter()
            .filter(|rec| participant_id("antigravity", &rec.child_id) == selected.participant)
            .collect();

        if matches.len() > 1 {
            return AgyChatOpen::Unavailable;
        }
        let Some(rec) = matches.pop() else {
            if post_fence()
                && confirm_open_custody(
                    repo,
                    selected.pid,
                    &selected.birth,
                    &owner,
                    uid,
                    &custody,
                    &brain_dir.trail.borrow(),
                    (&lead_file, &lead_stat),
                    None,
                    Some(&strict_ws),
                )
                .is_ok()
            {
                return AgyChatOpen::Unbound;
            }
            return AgyChatOpen::Unavailable;
        };

        let Ok((child_file, child_stat)) = open_transcript(
            &brain_dir,
            &rec.child_id,
            CHILD_BYTE_BUDGET as u64,
            uid,
            "child-transcript",
        ) else {
            return AgyChatOpen::Unavailable;
        };
        let Ok(child_jsonl) = read_child_full(&child_file, &child_stat) else {
            return AgyChatOpen::Unavailable;
        };
        let repo_uri = custody.repo.to_str().map(|p| format!("file://{p}"));
        let Some((chat, setup)) = extract_selected_child_chat_and_setup(
            &parent_jsonl,
            parent_recent,
            &child_jsonl,
            false,
            &custody.conversation_id,
            &selected.turn,
            &rec,
            repo_uri.as_deref(),
            |uri| strict_ws.accepts(uri),
        ) else {
            return AgyChatOpen::Unavailable;
        };
        let history = format_selected_child_reader(&setup, &chat);

        if !post_fence()
            || confirm_open_custody(
                repo,
                selected.pid,
                &selected.birth,
                &owner,
                uid,
                &custody,
                &brain_dir.trail.borrow(),
                (&lead_file, &lead_stat),
                Some((&child_file, &child_stat, None)),
                Some(&strict_ws),
            )
            .is_err()
        {
            return AgyChatOpen::Unavailable;
        }

        let Ok(lead_min_size) = u64::try_from(lead_stat.st_size) else {
            return AgyChatOpen::Unavailable;
        };
        let Ok(child_min_size) = u64::try_from(child_stat.st_size) else {
            return AgyChatOpen::Unavailable;
        };

        AgyChatOpen::OwnChat {
            history,
            locator: AgyChildChatLocator {
                pid: selected.pid,
                birth: owner.birth,
                app_root: custody.app_root,
                record: rec,
                lock: custody.lock,
                lead: (lead_stat.st_dev, lead_stat.st_ino),
                lead_min_size,
                child: (child_stat.st_dev, child_stat.st_ino),
                child_min_size,
            },
        }
    }

    pub(crate) fn agy_locator_still_current(repo: &Path, expected: &AgyChildChatLocator) -> bool {
        agy_locator_still_current_fenced(repo, expected, || true)
    }

    pub(crate) fn agy_locator_still_current_fenced(
        repo: &Path,
        expected: &AgyChildChatLocator,
        post_fence: impl FnOnce() -> bool,
    ) -> bool {
        agy_locator_still_current_stage(repo, expected, post_fence).is_ok()
    }

    pub(super) fn agy_locator_still_current_stage(
        repo: &Path,
        expected: &AgyChildChatLocator,
        post_fence: impl FnOnce() -> bool,
    ) -> Stage<()> {
        gate(
            is_uuid_lower_hex(&expected.record.lead_id)
                && is_uuid_lower_hex(&expected.record.child_id)
                && is_turn(&expected.record.turn),
            "locator",
        )?;
        let (uid, owner) = verify_process(expected.pid, &expected.birth)?;
        let custody = checked_lead_custody(expected.pid, &expected.birth, repo).ok_or("custody")?;
        gate(
            custody.pid == expected.pid
                && custody.birth == expected.birth
                && custody.app_root == expected.app_root
                && custody.conversation_id == expected.record.lead_id
                && custody.lock == expected.lock,
            "locator",
        )?;
        let app_dir = open_presence_app(&custody, uid, expected.lock)?;
        let brain_dir = app_dir.dir("brain", uid, "brain-dir")?;
        let (lead_file, lead_stat) = open_transcript(
            &brain_dir,
            &custody.conversation_id,
            TRANSCRIPT_LIMIT,
            uid,
            "lead-transcript",
        )?;
        gate(
            (lead_stat.st_dev, lead_stat.st_ino) == expected.lead
                && u64::try_from(lead_stat.st_size)
                    .is_ok_and(|size| size >= expected.lead_min_size),
            "lead-replaced",
        )?;
        let (parent_jsonl, parent_recent) = read_parent_tail(&lead_file, &lead_stat)?;

        let (child_file, child_stat) = open_transcript(
            &brain_dir,
            &expected.record.child_id,
            CHILD_BYTE_BUDGET as u64,
            uid,
            "child-transcript",
        )?;
        gate(
            (child_stat.st_dev, child_stat.st_ino) == expected.child
                && u64::try_from(child_stat.st_size).is_ok_and(|size| {
                    (expected.child_min_size..=CHILD_BYTE_BUDGET as u64).contains(&size)
                }),
            "child-replaced",
        )?;

        let mut strict_ws = AgyStrictWorkspaces::new(&custody.repo).ok_or("strict-workspace")?;
        #[cfg(test)]
        PARENT_SCANS.with(|n| n.set(n.get() + 1));
        let scan = scan_lead_turn(
            &parent_jsonl,
            parent_recent,
            &custody.conversation_id,
            Some(&expected.record.turn),
            |uri| strict_ws.accepts(uri),
        )
        .ok_or("lead-scan")?;
        gate(scan.turn == expected.record.turn, "lead-scan")?;
        let target_pid = participant_id("antigravity", &expected.record.child_id);
        let exact_matches = scan
            .children
            .iter()
            .filter(|rec| *rec == &expected.record)
            .count();
        let pid_matches = scan
            .children
            .iter()
            .filter(|rec| participant_id("antigravity", &rec.child_id) == target_pid)
            .count();
        gate(exact_matches == 1 && pid_matches == 1, "child-unmatched")?;
        gate(post_fence(), "post-fence")?;

        confirm_open_custody(
            repo,
            expected.pid,
            &expected.birth,
            &owner,
            uid,
            &custody,
            &brain_dir.trail.borrow(),
            (&lead_file, &lead_stat),
            Some((&child_file, &child_stat, Some(expected.child_min_size))),
            Some(&strict_ws),
        )
    }

    fn verify_process(pid: u32, birth: &str) -> Stage<(u32, Owner)> {
        gate(
            pid > 0 && pid <= i32::MAX as u32 && !birth.is_empty(),
            "process",
        )?;
        let uid = geteuid().as_raw();
        gate(getuid().as_raw() == uid, "process-owner")?;
        let owner = terminals::process(pid).map_err(|_| "process")?;
        gate(
            owner.pid == pid && owner.uid == uid && owner.birth == birth,
            "process-owner",
        )?;
        Ok((uid, owner))
    }

    fn open_presence_app(
        custody: &AgyLeadCustody,
        uid: u32,
        expected_lock: (fs::Dev, u64),
    ) -> Stage<Dir> {
        let app_dir = Dir::root(&custody.app_root, uid, "app-root")?;
        let presence_dir = app_dir.dir("presence", uid, "presence-dir")?;
        let (_, lock_stat) = presence_dir.file(
            &format!("{}.lock", custody.conversation_id),
            LOCK_FILE_LIMIT,
            uid,
            "presence-lock",
        )?;
        gate(
            (lock_stat.st_dev, lock_stat.st_ino) == expected_lock,
            "presence-lock",
        )?;
        Ok(app_dir)
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm_open_custody(
        repo: &Path,
        pid: u32,
        birth: &str,
        owner: &Owner,
        uid: u32,
        custody: &AgyLeadCustody,
        trail: &Trail,
        lead: (&File, &Stat),
        child: Option<(&File, &Stat, Option<u64>)>,
        strict_ws: Option<&AgyStrictWorkspaces>,
    ) -> Stage<()> {
        if let Some((child_file, child_stat, min_size)) = child {
            let child_now = fs::fstat(child_file).map_err(|_| "child-changed")?;
            let size_ok = match min_size {
                None => unchanged(child_stat, &child_now),
                Some(min) => {
                    (child_now.st_dev, child_now.st_ino) == (child_stat.st_dev, child_stat.st_ino)
                        && u64::try_from(child_now.st_size)
                            .is_ok_and(|size| (min..=CHILD_BYTE_BUDGET as u64).contains(&size))
                }
            };
            gate(
                size_ok && strict(&child_now, uid) && child_now.st_nlink == 1,
                "child-changed",
            )?;
        }
        let lead_now = fs::fstat(lead.0).map_err(|_| "lead-changed")?;
        gate(
            unchanged(lead.1, &lead_now) && strict(&lead_now, uid) && lead_now.st_nlink == 1,
            "lead-changed",
        )?;
        same_names(&custody.app_root, uid, trail)?;
        if let Some(ws) = strict_ws {
            gate(ws.confirm(), "workspace-changed")?;
        }
        gate(
            checked_lead_custody(pid, birth, repo).as_ref() == Some(custody),
            "custody-changed",
        )?;
        gate(
            terminals::process(pid).is_ok_and(|now| now == *owner),
            "process-changed",
        )?;
        Ok(())
    }

    fn strict(stat: &Stat, uid: u32) -> bool {
        stat.st_uid == uid && stat.st_mode & 0o022 == 0
    }

    fn is_uuid_lower_hex(s: &str) -> bool {
        let bytes = s.as_bytes();
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(idx, &b)| match idx {
                8 | 13 | 18 | 23 => b == b'-',
                _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
            })
    }

    #[derive(Default)]
    struct Trail(Vec<(Vec<String>, bool, (fs::Dev, u64))>);

    struct Dir {
        fd: OwnedFd,
        names: Vec<String>,
        trail: Rc<RefCell<Trail>>,
    }

    impl Dir {
        fn root(path: &Path, uid: u32, stage: &'static str) -> Stage<Self> {
            let _ = agy_dir_strict(path).ok_or(stage)?;
            let fd = open_dir(path).ok_or(stage)?;
            Self::admit(fd, Vec::new(), Rc::default(), uid, stage)
        }

        fn dir(&self, name: &str, uid: u32, stage: &'static str) -> Stage<Self> {
            gate(
                !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
                stage,
            )?;
            let fd = fs::openat(&self.fd, name, DIR, Mode::empty()).map_err(|_| stage)?;
            let mut names = self.names.clone();
            names.push(name.to_owned());
            Self::admit(fd, names, Rc::clone(&self.trail), uid, stage)
        }

        fn admit(
            fd: OwnedFd,
            names: Vec<String>,
            trail: Rc<RefCell<Trail>>,
            uid: u32,
            stage: &'static str,
        ) -> Stage<Self> {
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::Directory && strict(&stat, uid),
                stage,
            )?;
            trail
                .borrow_mut()
                .0
                .push((names.clone(), false, (stat.st_dev, stat.st_ino)));
            Ok(Self { fd, names, trail })
        }

        fn file(
            &self,
            name: &str,
            limit: u64,
            uid: u32,
            stage: &'static str,
        ) -> Stage<(File, Stat)> {
            gate(
                !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
                stage,
            )?;
            let fd = fs::openat(&self.fd, name, FILE, Mode::empty()).map_err(|_| stage)?;
            let stat = fs::fstat(&fd).map_err(|_| stage)?;
            gate(
                FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                    && stat.st_nlink == 1
                    && strict(&stat, uid)
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
    }

    fn same_names(app_root: &Path, uid: u32, trail: &Trail) -> Stage<()> {
        let _ = agy_dir_strict(app_root).ok_or("path-changed")?;
        let root = open_dir(app_root).ok_or("path-changed")?;
        for (names, is_file, ident) in &trail.0 {
            let mut fd = None;
            for (depth, name) in names.iter().enumerate() {
                let flags = if *is_file && depth + 1 == names.len() {
                    FILE
                } else {
                    DIR
                };
                let next = fs::openat(fd.as_ref().unwrap_or(&root), name, flags, Mode::empty())
                    .map_err(|_| "path-changed")?;
                fd = Some(next);
            }
            let now = fs::fstat(fd.as_ref().unwrap_or(&root)).map_err(|_| "path-changed")?;
            let expected_type = if *is_file {
                FileType::RegularFile
            } else {
                FileType::Directory
            };
            gate(
                FileType::from_raw_mode(now.st_mode) == expected_type
                    && strict(&now, uid)
                    && (!*is_file || now.st_nlink == 1)
                    && (now.st_dev, now.st_ino) == *ident,
                "path-changed",
            )?;
        }
        Ok(())
    }

    fn open_transcript(
        brain_dir: &Dir,
        uuid: &str,
        limit: u64,
        uid: u32,
        stage: &'static str,
    ) -> Stage<(File, Stat)> {
        gate(is_uuid_lower_hex(uuid), stage)?;
        brain_dir
            .dir(uuid, uid, stage)?
            .dir(".system_generated", uid, stage)?
            .dir("logs", uid, stage)?
            .file("transcript_full.jsonl", limit, uid, stage)
    }

    fn read_parent_tail(file: &File, stat: &Stat) -> Stage<(Vec<u8>, bool)> {
        let size = u64::try_from(stat.st_size).map_err(|_| "lead-read")?;
        gate(size > 0, "byte-budget")?;
        let budget = LEAD_BYTE_BUDGET as u64;
        let recent = size > budget;
        let offset = if recent { size - budget } else { 0 };
        let len = usize::try_from(size - offset).map_err(|_| "lead-read")?;
        let mut reader = file;
        reader
            .seek(SeekFrom::Start(offset))
            .map_err(|_| "lead-read")?;
        let mut body = Vec::with_capacity(len + 1);
        reader
            .take(len as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| "lead-read")?;
        gate(
            body.len() == len && fs::fstat(file).is_ok_and(|now| unchanged(stat, &now)),
            "lead-changed",
        )?;
        Ok((body, recent))
    }

    fn read_child_full(file: &File, stat: &Stat) -> Stage<Vec<u8>> {
        let size = u64::try_from(stat.st_size).map_err(|_| "child-read")?;
        gate(
            (1..=CHILD_BYTE_BUDGET as u64).contains(&size),
            "child-budget",
        )?;
        let len = usize::try_from(size).map_err(|_| "child-read")?;
        #[cfg(test)]
        CHILD_BODY_READS.with(|n| n.set(n.get() + 1));
        let mut reader = file;
        reader.seek(SeekFrom::Start(0)).map_err(|_| "child-read")?;
        let mut body = Vec::with_capacity(len + 1);
        reader
            .take(size + 1)
            .read_to_end(&mut body)
            .map_err(|_| "child-read")?;
        gate(
            body.len() == len && fs::fstat(file).is_ok_and(|now| unchanged(stat, &now)),
            "child-changed",
        )?;
        Ok(body)
    }

    pub(super) fn agy_current_children_stage(
        pid: u32,
        birth: &str,
        repo: &Path,
        expected_turn: Option<&str>,
        painted: &BTreeSet<String>,
    ) -> Stage<(String, Vec<AgyChildChatLocator>)> {
        gate(!painted.is_empty(), "no-painted")?;
        let (uid, owner) = verify_process(pid, birth)?;
        let custody = checked_lead_custody(pid, birth, repo).ok_or("custody")?;
        let app_dir = open_presence_app(&custody, uid, custody.lock)?;
        let brain_dir = app_dir.dir("brain", uid, "brain-dir")?;
        let (lead_file, lead_stat) = open_transcript(
            &brain_dir,
            &custody.conversation_id,
            TRANSCRIPT_LIMIT,
            uid,
            "lead-transcript",
        )?;
        let (parent_jsonl, parent_recent) = read_parent_tail(&lead_file, &lead_stat)?;
        let mut strict_ws = AgyStrictWorkspaces::new(&custody.repo).ok_or("strict-workspace")?;

        #[cfg(test)]
        PARENT_SCANS.with(|n| n.set(n.get() + 1));
        let scan = scan_lead_turn(
            &parent_jsonl,
            parent_recent,
            &custody.conversation_id,
            expected_turn,
            |uri| strict_ws.accepts(uri),
        )
        .ok_or("lead-scan")?;

        let mut participant_counts = BTreeMap::<String, usize>::new();
        for rec in &scan.children {
            *participant_counts
                .entry(participant_id("antigravity", &rec.child_id))
                .or_default() += 1;
        }

        let lead_min_size = u64::try_from(lead_stat.st_size).map_err(|_| "lead-read")?;
        let mut locators = Vec::new();
        for rec in scan.children {
            let participant = participant_id("antigravity", &rec.child_id);
            if !painted.contains(&participant)
                || participant_counts.get(&participant).copied() != Some(1)
            {
                continue;
            }
            let snap = app_dir.trail.borrow().0.len();
            let Ok((_, child_stat)) = open_transcript(
                &brain_dir,
                &rec.child_id,
                CHILD_BYTE_BUDGET as u64,
                uid,
                "child-transcript",
            ) else {
                app_dir.trail.borrow_mut().0.truncate(snap);
                continue;
            };
            let Ok(child_min_size) = u64::try_from(child_stat.st_size) else {
                app_dir.trail.borrow_mut().0.truncate(snap);
                continue;
            };
            locators.push(AgyChildChatLocator {
                pid,
                birth: owner.birth.clone(),
                app_root: custody.app_root.clone(),
                record: rec,
                lock: custody.lock,
                lead: (lead_stat.st_dev, lead_stat.st_ino),
                lead_min_size,
                child: (child_stat.st_dev, child_stat.st_ino),
                child_min_size,
            });
        }

        confirm_open_custody(
            repo,
            pid,
            birth,
            &owner,
            uid,
            &custody,
            &app_dir.trail.borrow(),
            (&lead_file, &lead_stat),
            None,
            Some(&strict_ws),
        )?;

        Ok((scan.turn, locators))
    }

    pub(super) fn agy_child_chat_fenced_stage(
        repo: &Path,
        turn: &str,
        expected: &AgyChildChatLocator,
        post_fence: impl FnOnce() -> bool,
    ) -> Stage<String> {
        gate(turn == expected.record.turn, "locator")?;
        gate(
            is_uuid_lower_hex(&expected.record.lead_id)
                && is_uuid_lower_hex(&expected.record.child_id),
            "locator",
        )?;
        let (uid, owner) = verify_process(expected.pid, &expected.birth)?;
        let custody = checked_lead_custody(expected.pid, &expected.birth, repo).ok_or("custody")?;
        gate(
            custody.pid == expected.pid
                && custody.birth == expected.birth
                && custody.app_root == expected.app_root
                && custody.conversation_id == expected.record.lead_id
                && custody.lock == expected.lock,
            "locator",
        )?;

        let app_dir = open_presence_app(&custody, uid, expected.lock)?;
        let brain_dir = app_dir.dir("brain", uid, "brain-dir")?;
        let (lead_file, lead_stat) = open_transcript(
            &brain_dir,
            &custody.conversation_id,
            TRANSCRIPT_LIMIT,
            uid,
            "lead-transcript",
        )?;
        gate(
            (lead_stat.st_dev, lead_stat.st_ino) == expected.lead,
            "lead-replaced",
        )?;
        let (parent_jsonl, parent_recent) = read_parent_tail(&lead_file, &lead_stat)?;

        let (child_file, child_stat) = open_transcript(
            &brain_dir,
            &expected.record.child_id,
            CHILD_BYTE_BUDGET as u64,
            uid,
            "child-transcript",
        )?;
        gate(
            (child_stat.st_dev, child_stat.st_ino) == expected.child,
            "child-replaced",
        )?;
        let child_jsonl = read_child_full(&child_file, &child_stat)?;

        let mut strict_ws = AgyStrictWorkspaces::new(&custody.repo).ok_or("strict-workspace")?;
        #[cfg(test)]
        PARENT_SCANS.with(|n| n.set(n.get() + 1));
        let chat = extract_child_chat(
            &parent_jsonl,
            parent_recent,
            &child_jsonl,
            false,
            &custody.conversation_id,
            turn,
            &expected.record,
            |uri| strict_ws.accepts(uri),
        )
        .ok_or("extract-failed")?;

        gate(post_fence(), "post-fence")?;

        confirm_open_custody(
            repo,
            expected.pid,
            &expected.birth,
            &owner,
            uid,
            &custody,
            &app_dir.trail.borrow(),
            (&lead_file, &lead_stat),
            Some((&child_file, &child_stat, None)),
            Some(&strict_ws),
        )?;

        Ok(chat)
    }
}

#[cfg(test)]
mod stub_tests {
    use super::{
        AgyChatOpen, AgyChildChatLocator, AgySelectedChild, unsupported_locator_current_fenced,
        unsupported_open_selected,
    };
    use crate::agy_child_records::AgyChildLocator;
    use std::path::PathBuf;

    #[test]
    fn off_linux_chat_refuses_custody_and_locator_supports_clone_and_eq() {
        assert!(!unsupported_locator_current_fenced(|| true));
        assert!(!unsupported_locator_current_fenced(|| false));
        assert!(matches!(
            unsupported_open_selected(|| true),
            AgyChatOpen::Unbound
        ));
        assert!(matches!(
            unsupported_open_selected(|| false),
            AgyChatOpen::Unavailable
        ));

        let sel = AgySelectedChild {
            pid: 42,
            birth: "boot:100".into(),
            turn: "0123456789abcdef0123".into(),
            participant: "native-antigravity-22222222-2222-2222-2222-bbbbbbbbbbbb".into(),
        };
        let sel_clone = sel.clone();
        assert!(sel == sel_clone);
        let mut sel_diff = sel_clone;
        sel_diff.pid = 43;
        assert!(sel != sel_diff);

        let a = AgyChildChatLocator {
            pid: 42,
            birth: "boot:100".into(),
            app_root: PathBuf::from("/tmp/app"),
            record: AgyChildLocator {
                lead_id: "aaaaaaaa-1111-2222-3333-444444444444".into(),
                turn: "0123456789abcdef0123".into(),
                child_id: "22222222-2222-2222-2222-bbbbbbbbbbbb".into(),
                spawn_step: 2,
                result_step: 3,
                boundary: "boundary".into(),
            },
            lock: (1, 2),
            lead: (1, 3),
            lead_min_size: 10,
            child: (1, 4),
            child_min_size: 20,
        };
        let b = a.clone();
        assert!(a == b);
        let mut c = b;
        c.pid = 43;
        assert!(a != c);

        #[cfg(not(target_os = "linux"))]
        {
            let repo = std::path::Path::new(".");
            assert!(matches!(
                super::agy_open_selected_child_fenced(repo, &sel, || true),
                AgyChatOpen::Unbound
            ));
            assert!(matches!(
                super::agy_open_selected_child_fenced(repo, &sel, || false),
                AgyChatOpen::Unavailable
            ));
            assert!(!super::agy_locator_still_current_fenced(repo, &a, || true));
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{
        CHILD_BODY_READS, PARENT_SCANS, Stage, agy_child_chat_fenced_stage,
        agy_current_children_stage,
    };
    use super::{
        AgyChatOpen, AgyChildChatLocator, AgySelectedChild, agy_child_chat_fenced,
        agy_child_read_counts, agy_current_children, agy_locator_still_current,
        agy_locator_still_current_fenced, agy_open_selected_child_fenced, reset_all_agy_counters,
        reset_test_counters, test_counters,
    };
    use crate::agy_child_records::{CHILD_BYTE_BUDGET, LEAD_BYTE_BUDGET};
    use crate::native_children::{TASK_PREFIX, participant_id};
    use oms_panel_core::{Value, terminals};
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const LEAD_ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
    const CHILD_A: &str = "22222222-2222-2222-2222-bbbbbbbbbbbb";
    const CHILD_B: &str = "33333333-3333-3333-3333-cccccccccccc";
    const CHILD_C: &str = "44444444-4444-4444-4444-dddddddddddd";
    const TURN_OLD: &str = "abcdef1234567890abcd";
    const TURN_CUR: &str = "1234567890abcdef1234";
    const SYSTEM_PREAMBLE: &str = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
    const CHECKPOINT_PREFIX: &str = "# Resuming from a compaction";
    const SUBAGENT_PREFIX: &str = "Created the following subagent:\n";
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    fn mode(path: &Path, perms: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(perms)).unwrap();
    }

    fn mkdir_private(path: &Path) {
        if fs::symlink_metadata(path).is_ok() {
            return;
        }
        if let Some(parent) = path.parent() {
            mkdir_private(parent);
        }
        let _ = fs::create_dir(path);
        mode(path, 0o700);
    }

    fn write_private(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            mkdir_private(parent);
        }
        fs::write(path, body).unwrap();
        mode(path, 0o600);
    }

    fn set_tree_mode(path: &Path) {
        let meta = fs::symlink_metadata(path).unwrap();
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            mode(path, 0o700);
            for entry in fs::read_dir(path).unwrap() {
                set_tree_mode(&entry.unwrap().path());
            }
        } else {
            mode(path, 0o600);
        }
    }

    fn copy_tree_exact(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let src = entry.path();
            let dst = to.join(entry.file_name());
            if src.is_dir() {
                copy_tree_exact(&src, &dst);
            } else {
                fs::copy(&src, &dst).unwrap();
                mode(
                    &dst,
                    fs::metadata(&src).unwrap().permissions().mode() & 0o777,
                );
            }
        }
        mode(to, fs::metadata(from).unwrap().permissions().mode() & 0o777);
    }

    fn uri(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    fn turn_req(turn: &str) -> String {
        format!(
            "<USER_REQUEST>\n{TASK_PREFIX} Turn {turn}. You own this provider's work. Extra task\n</USER_REQUEST>"
        )
    }

    fn user_row(step: u64, content: &str) -> Value {
        json!({"step_index": step, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "content": content})
    }

    fn planner_row(
        step: u64,
        content: Option<&str>,
        thinking: Option<&str>,
        calls: Option<Value>,
    ) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("step_index".into(), json!(step));
        obj.insert("source".into(), json!("MODEL"));
        obj.insert("type".into(), json!("PLANNER_RESPONSE"));
        obj.insert("status".into(), json!("DONE"));
        if let Some(c) = content {
            obj.insert("content".into(), json!(c));
        }
        if let Some(t) = thinking {
            obj.insert("thinking".into(), json!(t));
        }
        if let Some(tc) = calls {
            obj.insert("tool_calls".into(), tc);
        }
        Value::Object(obj)
    }

    fn generic_row(step: u64, content: &str) -> Value {
        json!({"step_index": step, "source": "MODEL", "type": "GENERIC", "status": "DONE", "content": content})
    }

    fn sys_msg_row(step: u64, content: &str) -> Value {
        json!({"step_index": step, "source": "SYSTEM", "type": "SYSTEM_MESSAGE", "status": "DONE", "content": content})
    }

    fn checkpoint_row(step: u64, summary: &str) -> Value {
        json!({"step_index": step, "source": "SYSTEM", "type": "CHECKPOINT", "status": "DONE", "content": format!("{CHECKPOINT_PREFIX}\n{summary}")})
    }

    fn invoke_call(id: Option<&str>, prompt: &str, role: &str, kind: &str) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(id) = id {
            obj.insert("id".into(), json!(id));
        }
        obj.insert("name".into(), json!("invoke_subagent"));
        obj.insert(
            "args".into(),
            json!({"Prompt": prompt, "Role": role, "TypeName": kind}),
        );
        Value::Object(obj)
    }

    fn send_call(id: &str, recipient: &str, msg: &str) -> Value {
        json!({"id": id, "name": "send_message", "args": {"Recipient": recipient, "Message": msg}})
    }

    fn subagent_created(step: u64, child_id: &str, uris: &[&str]) -> Value {
        let body = json!({
            "conversationId": child_id,
            "logAbsoluteUri": format!("file:///untrusted/ignored/brain/{child_id}/.system_generated/logs/transcript.jsonl"),
            "workspaceUris": uris,
        });
        generic_row(step, &format!("{SUBAGENT_PREFIX}{body:#}"))
    }

    fn msg_envelope(sender: &str, body: &str, preamble: bool) -> String {
        let p = if preamble { SYSTEM_PREAMBLE } else { "" };
        format!(
            "{p}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:00Z sender={sender} priority=HIGH content={body}\n</SYSTEM_MESSAGE>"
        )
    }

    fn to_jsonl(rows: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for row in rows {
            out.extend_from_slice(row.to_string().as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn shell_quote(raw: &str) -> String {
        format!("'{}'", raw.replace('\'', "'\"'\"'"))
    }

    struct Holder {
        child: Child,
        pid: u32,
        birth: String,
        stdin: Option<ChildStdin>,
        killed_early: bool,
    }

    impl Holder {
        fn spawn(home: &Path, lock_file: &Path, cwd: &Path) -> Self {
            let mut cmd = Command::new("flock");
            cmd.arg("-n")
                .arg("-F")
                .arg(lock_file)
                .args(["bash", "-c", "exec -a agy bash"])
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .env("HOME", home)
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut reader = BufReader::new(child.stdout.take().unwrap());
            writeln!(stdin, "echo \"READY:$$\"").unwrap();
            stdin.flush().unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let pid: u32 = line.trim().strip_prefix("READY:").unwrap().parse().unwrap();
            let birth = terminals::process(pid).unwrap().birth;
            Self {
                child,
                pid,
                birth,
                stdin: Some(stdin),
                killed_early: false,
            }
        }

        fn kill_and_wait(&mut self) {
            self.stdin.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.killed_early = true;
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            self.stdin.take();
            if !self.killed_early {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    struct Fixture {
        base: PathBuf,
        repo_parent: PathBuf,
        repo: PathBuf,
        wt_parent: PathBuf,
        wt: PathBuf,
        foreign_wt: PathBuf,
        home: PathBuf,
        gemini: PathBuf,
        app_root: PathBuf,
        presence: PathBuf,
        brain: PathBuf,
        holder: Option<Holder>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "oms-agy-child-chat-{}-{seq}-{label}",
                std::process::id()
            ));
            mkdir_private(&base);
            let repo_parent = base.join("repo-parent");
            let repo = repo_parent.join("repo");
            let wt_parent = base.join("wt-parent");
            let wt = wt_parent.join("repo-wt");
            let foreign = base.join("foreign");
            let foreign_wt = base.join("foreign-wt");
            let home = base.join("home");
            let gemini = home.join(".gemini");
            let app_root = gemini.join("antigravity-cli");
            let presence = app_root.join("presence");
            let brain = app_root.join("brain");
            for d in [
                &repo_parent,
                &repo,
                &wt_parent,
                &foreign,
                &home,
                &gemini,
                &app_root,
                &presence,
                &brain,
            ] {
                mkdir_private(d);
            }
            let fixture = Self {
                base: base.clone(),
                repo_parent,
                repo: repo.clone(),
                wt_parent,
                wt: wt.clone(),
                foreign_wt: foreign_wt.clone(),
                home: home.clone(),
                gemini,
                app_root,
                presence,
                brain,
                holder: None,
            };
            fixture.git(&repo, &["init", "-q"]);
            fixture.git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
            fixture.git(
                &repo,
                &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
            );
            fixture.git(&foreign, &["init", "-q"]);
            fixture.git(&foreign, &["commit", "-q", "--allow-empty", "-m", "init"]);
            fixture.git(
                &foreign,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "fwt",
                    foreign_wt.to_str().unwrap(),
                ],
            );
            set_tree_mode(&fixture.base);
            write_private(&fixture.lock_path(LEAD_ID), b"");
            fixture
        }

        fn git(&self, dir: &Path, args: &[&str]) {
            let mut command = Command::new("git");
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
                .env("HOME", &self.home)
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

        fn lock_path(&self, id: &str) -> PathBuf {
            self.presence.join(format!("{id}.lock"))
        }

        fn transcript_path(&self, id: &str) -> PathBuf {
            self.brain
                .join(id)
                .join(".system_generated/logs/transcript_full.jsonl")
        }

        fn write_transcript(&self, id: &str, rows: &[Value]) {
            write_private(&self.transcript_path(id), &to_jsonl(rows));
        }

        fn start_holder(&mut self) -> (u32, String) {
            let holder = Holder::spawn(&self.home, &self.lock_path(LEAD_ID), &self.repo);
            let pid = holder.pid;
            let birth = holder.birth.clone();
            self.holder = Some(holder);
            (pid, birth)
        }

        fn seed_standard(&mut self) -> (u32, String, BTreeSet<String>) {
            let (pid, birth) = self.start_holder();
            let p_a = "analyze crates";
            let p_b = "verify sibling";
            let lead_rows = vec![
                user_row(0, &turn_req(TURN_OLD)),
                planner_row(
                    1,
                    Some("old spawn"),
                    None,
                    Some(json!([invoke_call(
                        Some("t0"),
                        "old prompt",
                        "researcher",
                        "general"
                    )])),
                ),
                subagent_created(2, CHILD_C, &[&uri(&self.repo)]),
                checkpoint_row(3, "compacted"),
                user_row(4, &turn_req(TURN_CUR)),
                planner_row(
                    5,
                    Some("PARENT_NARRATION_MUST_NOT_LEAK"),
                    None,
                    Some(json!([
                        invoke_call(Some("t1"), p_a, "researcher", "general"),
                        invoke_call(Some("t2"), p_b, "reviewer", "general"),
                    ])),
                ),
                subagent_created(6, CHILD_A, &[&uri(&self.repo), &uri(&self.wt)]),
                subagent_created(7, CHILD_B, &[&uri(&self.wt)]),
                sys_msg_row(
                    8,
                    &msg_envelope(CHILD_A, "PARENT_SUMMARY_MUST_NOT_LEAK", true),
                ),
            ];
            let child_a_rows = vec![
                sys_msg_row(0, &msg_envelope(LEAD_ID, p_a, true)),
                planner_row(
                    1,
                    Some("CHILD_FIRST_REPLY"),
                    Some("SECRET_THINKING_MUST_NOT_LEAK"),
                    Some(json!([{"id": "read1", "name": "read_file", "args": {"Path": "/x"}}])),
                ),
                generic_row(2, "TOOL_IO_MUST_NOT_LEAK"),
                checkpoint_row(3, "CHILD_CHECKPOINT_MUST_NOT_LEAK"),
                sys_msg_row(
                    4,
                    &msg_envelope(&format!("{CHILD_A}/task-1"), "WAKEUP_MUST_NOT_LEAK", true),
                ),
                planner_row(
                    5,
                    Some("CHILD_SECOND_REPLY"),
                    None,
                    Some(json!([send_call(
                        "s1",
                        LEAD_ID,
                        "CHILD_CONFIRMED_HANDBACK"
                    )])),
                ),
                generic_row(
                    6,
                    &format!(
                        "Created At: 2026-11-18T18:01:00Z\nCompleted At: 2026-11-18T18:01:01Z\nMessage sent to \"{LEAD_ID}\"."
                    ),
                ),
            ];
            let child_b_rows = vec![
                sys_msg_row(0, &msg_envelope(LEAD_ID, p_b, true)),
                planner_row(1, Some("SIBLING_REPLY_MUST_NOT_LEAK"), None, None),
            ];
            self.write_transcript(LEAD_ID, &lead_rows);
            self.write_transcript(CHILD_A, &child_a_rows);
            self.write_transcript(CHILD_B, &child_b_rows);
            let painted = BTreeSet::from([participant_id("antigravity", CHILD_A)]);
            (pid, birth, painted)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.holder.take();
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn happy_path_scan_bounds_and_redaction() {
        let mut fixture = Fixture::new("happy");
        let (pid, birth, painted) = fixture.seed_standard();
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        // Inject a secret token into child A's reply; sibling B stays unreadable garbage to prove B is never read.
        let mut a_rows = vec![
            sys_msg_row(0, &msg_envelope(LEAD_ID, "analyze crates", true)),
            planner_row(1, Some("CHILD_FIRST_REPLY"), Some("SECRET_THINKING"), None),
            checkpoint_row(2, "CHILD_CHECKPOINT"),
            sys_msg_row(
                3,
                &msg_envelope(&format!("{CHILD_A}/task-1"), "WAKEUP_NO_LEAK", true),
            ),
            planner_row(
                4,
                Some(&format!("CHILD_SECOND_REPLY\nleak {sample}")),
                None,
                Some(json!([send_call(
                    "s1",
                    LEAD_ID,
                    "CHILD_CONFIRMED_HANDBACK"
                )])),
            ),
            generic_row(5, &format!("Message sent to \"{LEAD_ID}\".")),
        ];
        fixture.write_transcript(CHILD_A, &a_rows);
        write_private(&fixture.transcript_path(CHILD_B), b"not-valid-json\n");

        PARENT_SCANS.with(|n| n.set(0));
        CHILD_BODY_READS.with(|n| n.set(0));
        let (turn, locators) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        assert_eq!(PARENT_SCANS.with(|n| n.get()), 1);
        assert_eq!(CHILD_BODY_READS.with(|n| n.get()), 0);
        assert_eq!(turn, TURN_CUR);
        assert_eq!(locators.len(), 1);
        let loc = &locators[0];

        PARENT_SCANS.with(|n| n.set(0));
        CHILD_BODY_READS.with(|n| n.set(0));
        let chat = agy_child_chat_fenced(&fixture.repo, TURN_CUR, loc, || true).unwrap();
        assert_eq!(PARENT_SCANS.with(|n| n.get()), 1);
        assert_eq!(CHILD_BODY_READS.with(|n| n.get()), 1);

        assert!(chat.contains("CHILD_FIRST_REPLY"));
        assert!(chat.contains("CHILD_SECOND_REPLY"));
        assert!(chat.contains("CHILD_CONFIRMED_HANDBACK"));
        assert!(chat.contains("[oms-redacted:github-token]") && !chat.contains(&sample));
        for forbidden in [
            "SECRET_THINKING",
            "CHILD_CHECKPOINT",
            "WAKEUP_NO_LEAK",
            "PARENT_NARRATION_MUST_NOT_LEAK",
            "PARENT_SUMMARY_MUST_NOT_LEAK",
            "SIBLING_REPLY_MUST_NOT_LEAK",
        ] {
            assert!(!chat.contains(forbidden), "leaked {forbidden}: {chat}");
        }
        let first_idx = chat.find("CHILD_FIRST_REPLY").unwrap();
        let second_idx = chat.find("CHILD_SECOND_REPLY").unwrap();
        let handback_idx = chat.find("CHILD_CONFIRMED_HANDBACK").unwrap();
        assert!(first_idx < second_idx && second_idx < handback_idx);

        // Removing the child transcript does not affect other painted checks when unselected.
        a_rows.clear();
        let _ = a_rows;
    }

    #[test]
    fn oversize_parent_tail_passes_and_oversize_child_fails() {
        let mut fixture = Fixture::new("budgets");
        let (pid, birth, painted) = fixture.seed_standard();

        // > 4 MiB parent tail (`parent_recent == true`) still binds and reads child A.
        let mut big_lead = Vec::new();
        let pad = "p".repeat(60_000);
        let filler_rows = (LEAD_BYTE_BUDGET / 60_000) + 10;
        for step in 0..filler_rows as u64 {
            big_lead.push(planner_row(step, Some(&pad), None, None));
        }
        let base_step = filler_rows as u64;
        big_lead.extend([
            user_row(base_step, &turn_req(TURN_CUR)),
            planner_row(
                base_step + 1,
                Some("spawn"),
                None,
                Some(json!([invoke_call(
                    Some("t1"),
                    "analyze crates",
                    "researcher",
                    "general"
                )])),
            ),
            subagent_created(base_step + 2, CHILD_A, &[&uri(&fixture.repo)]),
        ]);
        let lead_bytes = to_jsonl(&big_lead);
        assert!(lead_bytes.len() > LEAD_BYTE_BUDGET);
        write_private(&fixture.transcript_path(LEAD_ID), &lead_bytes);

        let (_, locators) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        assert_eq!(locators.len(), 1);
        let loc = locators[0].clone();
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &loc, || true).is_some());

        // > 1 MiB child fails both agy_current_children (not bound) and agy_child_chat_fenced.
        let big_pad = "c".repeat(60_000);
        let mut big_child = vec![sys_msg_row(
            0,
            &msg_envelope(LEAD_ID, "analyze crates", true),
        )];
        for step in 1..=((CHILD_BYTE_BUDGET / 60_000) as u64 + 5) {
            big_child.push(planner_row(step, Some(&big_pad), None, None));
        }
        let child_bytes = to_jsonl(&big_child);
        assert!(child_bytes.len() > CHILD_BYTE_BUDGET);
        write_private(&fixture.transcript_path(CHILD_A), &child_bytes);

        let (_, empty) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        assert!(empty.is_empty());
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &loc, || true).is_none());
    }

    #[test]
    fn strict_fail_closed_matrix_refuses_tampering_and_midflight_changes() {
        // 1. Empty painted returns None without scanning.
        let mut fixture = Fixture::new("matrix");
        let (pid, birth, painted) = fixture.seed_standard();
        assert_eq!(
            agy_current_children_stage(
                pid,
                &birth,
                &fixture.repo,
                Some(TURN_CUR),
                &BTreeSet::new()
            )
            .err(),
            Some("no-painted")
        );
        assert!(
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &BTreeSet::new())
                .is_none()
        );
        let (_, locators) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        let valid_loc = locators[0].clone();
        assert_eq!(valid_loc.app_root, fixture.app_root);

        // 2. 0775 .git, 0775 repo-wt, 0775 .gemini, 0775 parent dirs above repo & repo-wt.
        for (path, bad_mode, good_mode) in [
            (fixture.repo.join(".git"), 0o775, 0o700),
            (fixture.wt.clone(), 0o775, 0o700),
            (fixture.gemini.clone(), 0o775, 0o700),
            (fixture.repo_parent.clone(), 0o775, 0o700),
            (fixture.wt_parent.clone(), 0o775, 0o700),
        ] {
            mode(&path, bad_mode);
            let res = agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted);
            assert!(
                res.as_ref().is_none_or(|(_, v)| v.is_empty()),
                "mode {path:?}"
            );
            assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
            mode(&path, good_mode);
        }
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_some());

        // 3. Foreign worktree child is refused.
        let foreign_painted = BTreeSet::from([participant_id("antigravity", CHILD_B)]);
        let orig_lead = fs::read(fixture.transcript_path(LEAD_ID)).unwrap();
        let mut lead_foreign = to_jsonl(&[
            user_row(0, &turn_req(TURN_CUR)),
            planner_row(
                1,
                Some("spawn"),
                None,
                Some(json!([invoke_call(
                    Some("t1"),
                    "verify sibling",
                    "researcher",
                    "general"
                )])),
            ),
            subagent_created(2, CHILD_B, &[&uri(&fixture.foreign_wt)]),
        ]);
        write_private(&fixture.transcript_path(LEAD_ID), &lead_foreign);
        let (_, foreign_locs) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &foreign_painted)
                .unwrap();
        assert!(foreign_locs.is_empty());
        lead_foreign.clear();
        let _ = lead_foreign;
        write_private(&fixture.transcript_path(LEAD_ID), &orig_lead);

        // 4. Sibling child locator mismatch / wrong locator fields.
        let mut bad_locs = Vec::<(&str, AgyChildChatLocator)>::new();
        let mut l = valid_loc.clone();
        l.pid += 1;
        bad_locs.push(("pid", l));
        let mut l = valid_loc.clone();
        l.birth = "wrong:1".into();
        bad_locs.push(("birth", l));
        let mut l = valid_loc.clone();
        l.app_root = fixture.base.join("other-app");
        bad_locs.push(("app_root", l));
        let mut l = valid_loc.clone();
        l.lock.1 += 1;
        bad_locs.push(("lock", l));
        let mut l = valid_loc.clone();
        l.lead.1 += 1;
        bad_locs.push(("lead", l));
        let mut l = valid_loc.clone();
        l.child.1 += 1;
        bad_locs.push(("child", l));
        let mut l = valid_loc.clone();
        l.record.child_id = CHILD_B.into();
        bad_locs.push(("sibling-child", l));
        let mut l = valid_loc.clone();
        l.record.turn = TURN_OLD.into();
        bad_locs.push(("wrong-turn-field", l));
        let mut l = valid_loc.clone();
        l.record.boundary = "0".repeat(64);
        bad_locs.push(("boundary", l));
        for (label, bad) in bad_locs {
            assert!(
                agy_child_chat_fenced(&fixture.repo, TURN_CUR, &bad, || true).is_none(),
                "{label}"
            );
        }
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_OLD, &valid_loc, || true).is_none());

        // 5. Symlink on child transcript & hardlink on child/lead transcripts fail.
        let child_path = fixture.transcript_path(CHILD_A);
        let twin = fixture.base.join("twin");
        fs::hard_link(&child_path, &twin).unwrap();
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        fs::remove_file(&twin).unwrap();

        let lead_path = fixture.transcript_path(LEAD_ID);
        fs::hard_link(&lead_path, &twin).unwrap();
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        fs::remove_file(&twin).unwrap();

        let moved_child = fixture.base.join("moved-child");
        fs::rename(&child_path, &moved_child).unwrap();
        symlink(&moved_child, &child_path).unwrap();
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        fs::remove_file(&child_path).unwrap();
        fs::rename(&moved_child, &child_path).unwrap();

        let moved_brain = fixture.base.join("moved-brain");
        fs::rename(&fixture.brain, &moved_brain).unwrap();
        symlink(&moved_brain, &fixture.brain).unwrap();
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        fs::remove_file(&fixture.brain).unwrap();
        fs::rename(&moved_brain, &fixture.brain).unwrap();

        // 6. Partial line missing `\n` and `truncated_fields` fail.
        let orig_child = fs::read(&child_path).unwrap();
        let mut partial = orig_child.clone();
        partial.extend_from_slice(br#"{"step_index":99,"source":"MODEL""#);
        write_private(&child_path, &partial);
        assert!(agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &valid_loc, || true).is_err());

        let mut trunc = orig_child.clone();
        trunc.extend_from_slice(
            br#"{"step_index":7,"source":"MODEL","type":"GENERIC","status":"DONE","content":"x","truncated_fields":["content"]}"#,
        );
        trunc.push(b'\n');
        write_private(&child_path, &trunc);
        assert!(agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &valid_loc, || true).is_err());
        write_private(&child_path, &orig_child);

        // 7. Newer Turn / newer USER_INPUT fails.
        let mut newer_lead = orig_lead.clone();
        newer_lead.extend_from_slice(&to_jsonl(&[user_row(
            9,
            "<USER_REQUEST>\nmanual prompt\n</USER_REQUEST>",
        )]));
        write_private(&lead_path, &newer_lead);
        assert!(
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).is_none()
        );
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());

        // Child SYSTEM_MESSAGE continuation from lead_id also suppresses child replies.
        let mut child_cont = orig_child.clone();
        child_cont.extend_from_slice(&to_jsonl(&[sys_msg_row(
            7,
            &msg_envelope(LEAD_ID, "follow-up from lead", true),
        )]));
        write_private(&child_path, &child_cont);
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        write_private(&child_path, &orig_child);

        // Newer Turn also invalidates stale Turn locator.
        let mut newer_turn = orig_lead.clone();
        newer_turn.extend_from_slice(&to_jsonl(&[user_row(9, &turn_req("99999999999999999999"))]));
        write_private(&lead_path, &newer_turn);
        assert!(
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).is_none()
        );
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());

        // 8. send_message-continued child (from lead) is suppressed.
        let mut sent_lead = orig_lead.clone();
        sent_lead.extend_from_slice(&to_jsonl(&[
            planner_row(
                9,
                Some("follow up"),
                None,
                Some(json!([send_call("s2", CHILD_A, "continue")])),
            ),
            generic_row(10, "Message sent."),
        ]));
        write_private(&lead_path, &sent_lead);
        let (_, after_send) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        assert!(after_send.is_empty());
        assert!(agy_child_chat_fenced(&fixture.repo, TURN_CUR, &valid_loc, || true).is_none());
        write_private(&lead_path, &orig_lead);

        // 9. Mid-flight post_fence mutations: false fence, file append, inode swap, rename, chmod, process change.
        assert_eq!(
            agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &valid_loc, || false).err(),
            Some("post-fence")
        );

        let child_append = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &valid_loc, || {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&child_path)
                .unwrap();
            f.write_all(&to_jsonl(&[planner_row(7, Some("late reply"), None, None)]))
                .unwrap();
            true
        });
        assert_eq!(child_append.err(), Some("child-changed"));
        write_private(&child_path, &orig_child);

        let lead_append = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &valid_loc, || {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&to_jsonl(&[planner_row(9, Some("late"), None, None)]))
                .unwrap();
            true
        });
        assert_eq!(lead_append.err(), Some("lead-changed"));
        write_private(&lead_path, &orig_lead);

        // Re-bind fresh locators after restoring lead/child mtimes/sizes.
        let (_, fresh_locs) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        let fresh_loc = fresh_locs[0].clone();

        // Mid-flight inode swap of child transcript.
        let swap_child = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
            let tmp = fixture.base.join("new-child.jsonl");
            write_private(&tmp, &orig_child);
            fs::rename(&tmp, &child_path).unwrap();
            true
        });
        assert_eq!(swap_child.err(), Some("child-changed"));

        let (_, fresh_locs) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        let fresh_loc = fresh_locs[0].clone();

        // Mid-flight rename/recreate of brain directory.
        let swap_brain = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
            let away = fixture.base.join("away-brain");
            fs::rename(&fixture.brain, &away).unwrap();
            copy_tree_exact(&away, &fixture.brain);
            true
        });
        assert_eq!(swap_brain.err(), Some("path-changed"));

        let (_, fresh_locs) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        let fresh_loc = fresh_locs[0].clone();

        // Mid-flight chmod on worktree.
        let chmod_wt = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
            mode(&fixture.wt, 0o775);
            true
        });
        assert_eq!(chmod_wt.err(), Some("workspace-changed"));
        mode(&fixture.wt, 0o700);

        // Mid-flight chmod on .gemini.
        let chmod_gemini = agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
            mode(&fixture.gemini, 0o775);
            true
        });
        assert_eq!(chmod_gemini.err(), Some("path-changed"));
        mode(&fixture.gemini, 0o700);

        // Mid-flight custody change via forbidden settings.json override.
        let settings_path = fixture.app_root.join("settings.json");
        let custody_override =
            agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
                write_private(
                    &settings_path,
                    br#"{"remoteUrl":"https://example.invalid"}"#,
                );
                true
            });
        assert_eq!(custody_override.err(), Some("custody-changed"));
        let _ = fs::remove_file(&settings_path);

        // Mid-flight process exit/change.
        let proc_exit: Stage<String> =
            agy_child_chat_fenced_stage(&fixture.repo, TURN_CUR, &fresh_loc, || {
                fixture.holder.as_mut().unwrap().kill_and_wait();
                true
            });
        assert!(proc_exit.is_err());

        let _ = shell_quote("x");
    }

    #[test]
    fn agy_open_selected_child_distinguishes_own_chat_unbound_and_unavailable() {
        let mut fixture = Fixture::new("open-selected");
        let (pid, birth, _) = fixture.seed_standard();
        let pid_a = participant_id("antigravity", CHILD_A);
        let pid_b = participant_id("antigravity", CHILD_B);
        let pid_c = participant_id("antigravity", CHILD_C);
        let sel_a = AgySelectedChild {
            pid,
            birth: birth.clone(),
            turn: TURN_CUR.into(),
            participant: pid_a,
        };
        let sel_b = AgySelectedChild {
            pid,
            birth: birth.clone(),
            turn: TURN_CUR.into(),
            participant: pid_b,
        };
        let sel_c = AgySelectedChild {
            pid,
            birth: birth.clone(),
            turn: TURN_CUR.into(),
            participant: pid_c,
        };

        // 1. Happy path -> OwnChat with exact (custody=2, scan=1, read=1) and min sizes.
        reset_test_counters();
        let AgyChatOpen::OwnChat { history, locator } =
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected OwnChat on strict 0700 child");
        };
        assert_eq!(test_counters(), (2, 1, 1));
        assert!(history.contains("CHILD_FIRST_REPLY"));
        assert!(history.contains("CHILD_CONFIRMED_HANDBACK"));
        assert!(!history.contains("PARENT_SUMMARY_MUST_NOT_LEAK"));
        assert_eq!(
            locator.lead_min_size,
            fs::metadata(fixture.transcript_path(LEAD_ID))
                .unwrap()
                .len()
        );
        assert_eq!(
            locator.child_min_size,
            fs::metadata(fixture.transcript_path(CHILD_A))
                .unwrap()
                .len()
        );

        // 2. Unbound fallback paths (0 child reads) when live board fence holds, Unavailable if fence fails.
        for (path, bad_mode, target) in [
            (fixture.repo.join(".git"), 0o775, &sel_a),
            (fixture.wt.clone(), 0o775, &sel_b),
            (fixture.gemini.clone(), 0o775, &sel_a),
            (fixture.brain.clone(), 0o775, &sel_a),
        ] {
            mode(&path, bad_mode);
            reset_test_counters();
            assert!(
                matches!(
                    agy_open_selected_child_fenced(&fixture.repo, target, || true),
                    AgyChatOpen::Unbound
                ),
                "expected Unbound for relaxed {path:?}"
            );
            assert_eq!(test_counters().2, 0, "expected 0 child reads for {path:?}");
            assert!(matches!(
                agy_open_selected_child_fenced(&fixture.repo, target, || false),
                AgyChatOpen::Unavailable
            ));
            mode(&path, 0o700);
        }

        // Prior-Turn child row (`CHILD_C` from `TURN_OLD`) -> Unbound (0 child reads).
        reset_test_counters();
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_c, || true),
            AgyChatOpen::Unbound
        ));
        assert_eq!(test_counters(), (2, 1, 0));

        // `send_message`-continued child (`CHILD_A`) -> Unbound (0 child reads).
        let orig_lead = fs::read(fixture.transcript_path(LEAD_ID)).unwrap();
        let mut sent_lead = orig_lead.clone();
        sent_lead.extend_from_slice(&to_jsonl(&[
            planner_row(
                9,
                Some("follow up"),
                None,
                Some(json!([send_call("s2", CHILD_A, "continue")])),
            ),
            generic_row(10, "Message sent."),
        ]));
        write_private(&fixture.transcript_path(LEAD_ID), &sent_lead);
        reset_test_counters();
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true),
            AgyChatOpen::Unbound
        ));
        assert_eq!(test_counters(), (2, 1, 0));
        write_private(&fixture.transcript_path(LEAD_ID), &orig_lead);

        // 3. Strict bound child failures MUST return Unavailable (NEVER Unbound!).
        let child_path = fixture.transcript_path(CHILD_A);
        let orig_child = fs::read(&child_path).unwrap();

        // 3a. "no-reply": child has prompt + thinking-only / empty assistant reply.
        fixture.write_transcript(
            CHILD_A,
            &[
                sys_msg_row(0, &msg_envelope(LEAD_ID, "analyze crates", true)),
                planner_row(1, None, Some("ONLY_THINKING"), None),
            ],
        );
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true),
            AgyChatOpen::Unavailable
        ));

        // 3b. `truncated_fields` in child transcript.
        let mut trunc = orig_child.clone();
        trunc.extend_from_slice(
            br#"{"step_index":7,"source":"MODEL","type":"GENERIC","status":"DONE","content":"x","truncated_fields":["content"]}"#,
        );
        trunc.push(b'\n');
        write_private(&child_path, &trunc);
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true),
            AgyChatOpen::Unavailable
        ));

        // 3c. Partial line (missing `\n`).
        let mut partial = orig_child.clone();
        partial.extend_from_slice(br#"{"step_index":99,"source":"MODEL""#);
        write_private(&child_path, &partial);
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true),
            AgyChatOpen::Unavailable
        ));
        write_private(&child_path, &orig_child);

        // 3d. Stale Turn and newer USER_INPUT -> Unavailable.
        let stale_sel = AgySelectedChild {
            turn: TURN_OLD.into(),
            ..sel_a.clone()
        };
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &stale_sel, || true),
            AgyChatOpen::Unavailable
        ));
        let mut newer_lead = orig_lead.clone();
        newer_lead.extend_from_slice(&to_jsonl(&[user_row(
            9,
            "<USER_REQUEST>\nmanual prompt\n</USER_REQUEST>",
        )]));
        write_private(&fixture.transcript_path(LEAD_ID), &newer_lead);
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true),
            AgyChatOpen::Unavailable
        ));
        write_private(&fixture.transcript_path(LEAD_ID), &orig_lead);

        // 3e. Post-fence failure and mid-flight mutation during strict open -> Unavailable.
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || false),
            AgyChatOpen::Unavailable
        ));
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || {
                mode(&fixture.wt, 0o775);
                true
            }),
            AgyChatOpen::Unavailable
        ));
        mode(&fixture.wt, 0o700);
        assert!(matches!(
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || {
                fixture.holder.as_mut().unwrap().kill_and_wait();
                true
            }),
            AgyChatOpen::Unavailable
        ));
    }

    #[test]
    fn agy_locator_still_current_validates_generation_and_fails_closed() {
        let mut fixture = Fixture::new("still-current");
        let (pid, birth, _) = fixture.seed_standard();
        let sel_a = AgySelectedChild {
            pid,
            birth,
            turn: TURN_CUR.into(),
            participant: participant_id("antigravity", CHILD_A),
        };
        let AgyChatOpen::OwnChat { locator, .. } =
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected initial OwnChat");
        };

        // 1. Live generation check succeeds with exact (custody=2, scan=1, child_reads=0),
        // and fenced false post_fence stops right before confirm_open_custody with (1, 1, 0).
        reset_test_counters();
        assert!(!agy_locator_still_current_fenced(
            &fixture.repo,
            &locator,
            || false
        ));
        assert_eq!(test_counters(), (1, 1, 0));
        reset_test_counters();
        assert!(agy_locator_still_current_fenced(
            &fixture.repo,
            &locator,
            || true
        ));
        assert_eq!(test_counters(), (2, 1, 0));
        reset_test_counters();
        assert!(agy_locator_still_current(&fixture.repo, &locator));
        assert_eq!(test_counters(), (2, 1, 0));

        let child_path = fixture.transcript_path(CHILD_A);
        let lead_path = fixture.transcript_path(LEAD_ID);
        let orig_child = fs::read(&child_path).unwrap();
        let orig_lead = fs::read(&lead_path).unwrap();

        // 2. Child inode swap fails even with identical bytes (rename replaces st_ino).
        let tmp_child = fixture.base.join("swapped-child.jsonl");
        write_private(&tmp_child, &orig_child);
        fs::rename(&tmp_child, &child_path).unwrap();
        assert!(!agy_locator_still_current(&fixture.repo, &locator));

        // Re-bind cleanly and verify lead inode swap fails.
        let AgyChatOpen::OwnChat { locator, .. } =
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected re-bound OwnChat");
        };
        let tmp_lead = fixture.base.join("swapped-lead.jsonl");
        write_private(&tmp_lead, &orig_lead);
        fs::rename(&tmp_lead, &lead_path).unwrap();
        assert!(!agy_locator_still_current(&fixture.repo, &locator));

        // Re-bind cleanly for size/content/custody invalidation checks.
        let AgyChatOpen::OwnChat { locator, .. } =
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected re-bound OwnChat");
        };

        // 3. In-place child/lead truncation (`size < min_size`) & > 1 MiB child growth fail.
        {
            let f = fs::OpenOptions::new()
                .write(true)
                .open(&child_path)
                .unwrap();
            f.set_len(locator.child_min_size - 1).unwrap();
        }
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
        write_private(&child_path, &orig_child);
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        {
            let f = fs::OpenOptions::new().write(true).open(&lead_path).unwrap();
            f.set_len(locator.lead_min_size - 1).unwrap();
        }
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
        write_private(&lead_path, &orig_lead);
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&child_path)
                .unwrap();
            let big = to_jsonl(&[planner_row(
                99,
                Some(&"x".repeat(CHILD_BYTE_BUDGET)),
                None,
                None,
            )]);
            f.write_all(&big).unwrap();
        }
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
        write_private(&child_path, &orig_child);
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        // 4. 0775 chmod on .git, wt, .gemini, brain, child_path fail.
        for path in [
            fixture.repo.join(".git"),
            fixture.wt.clone(),
            fixture.gemini.clone(),
            fixture.brain.clone(),
            child_path.clone(),
        ] {
            let restore = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            mode(&path, 0o775);
            assert!(
                !agy_locator_still_current(&fixture.repo, &locator),
                "0775 {path:?} must invalidate open dock"
            );
            mode(&path, restore);
        }
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        // 5. newer-user on lead fails.
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&to_jsonl(&[user_row(
                9,
                "<USER_REQUEST>\nmanual\n</USER_REQUEST>",
            )]))
            .unwrap();
        }
        assert!(!agy_locator_still_current(&fixture.repo, &locator));

        // Restore orig_lead in-place (truncate+rewrite same inode) -> valid again.
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&orig_lead).unwrap();
        }
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        // 6. send_message to CHILD_A on lead fails.
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&to_jsonl(&[
                planner_row(
                    9,
                    Some("follow up"),
                    None,
                    Some(json!([send_call("s2", CHILD_A, "continue")])),
                ),
                generic_row(10, "Message sent."),
            ]))
            .unwrap();
        }
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&orig_lead).unwrap();
        }
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        // 7. settings.json custody override fails.
        let settings_path = fixture.app_root.join("settings.json");
        write_private(
            &settings_path,
            br#"{"remoteUrl":"https://example.invalid"}"#,
        );
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
        fs::remove_file(&settings_path).unwrap();
        assert!(agy_locator_still_current(&fixture.repo, &locator));

        // 8. Killed process fails.
        fixture.holder.as_mut().unwrap().kill_and_wait();
        assert!(!agy_locator_still_current(&fixture.repo, &locator));
    }

    #[test]
    fn agy_open_selected_child_renders_recorded_setup_header() {
        let mut fixture = Fixture::new("setup-header");
        let (pid, birth, _) = fixture.seed_standard();
        let sel_a = AgySelectedChild {
            pid,
            birth: birth.clone(),
            turn: TURN_CUR.into(),
            participant: participant_id("antigravity", CHILD_A),
        };
        let AgyChatOpen::OwnChat { history, .. } =
            agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected OwnChat on strict 0700 child");
        };
        assert!(
            history.starts_with("Recorded AGY setup (OS/shell access unverified):\n"),
            "missing setup header in history: {history}"
        );
        assert!(history.contains("CHILD_FIRST_REPLY"));

        // Second fixture testing positive define_subagent + branch/inherit worktree links,
        // unconfigured header omission, >12 KiB long child reply preservation, zero reads
        // during passive checks, and preservation of open dock locator across appended
        // shared-worktree sibling or late/conflicting/failed define_subagent rows.
        let wt2 = fixture.wt_parent.join("repo-wt2");
        fixture.git(
            &fixture.repo,
            &["worktree", "add", "-q", "-b", "wt2", wt2.to_str().unwrap()],
        );
        set_tree_mode(&fixture.base);

        let p_builder = "implement scoped change";
        let p_research = "check docs";
        let p_unconfigured = "general work";
        let lead_rows = vec![
            user_row(0, &turn_req(TURN_CUR)),
            planner_row(
                1,
                None,
                None,
                Some(json!([{
                    "id": "d1",
                    "name": "define_subagent",
                    "args": {
                        "Name": "builder-worker",
                        "Description": "Writes code",
                        "SystemPrompt": "Stay in scope.",
                        "EnableWriteTools": true,
                        "EnableSubagentTools": false,
                    }
                }])),
            ),
            generic_row(
                2,
                "Created At: 2026-11-18T18:00:00Z\nSubagent \"builder-worker\" defined successfully. It can now be invoked via invoke_subagent.\n",
            ),
            planner_row(
                3,
                None,
                None,
                Some(json!([
                    {
                        "id": "s1",
                        "name": "invoke_subagent",
                        "args": {
                            "Prompt": p_builder,
                            "Role": "builder",
                            "TypeName": "builder-worker",
                            "Model": "pro",
                            "Workspace": "branch",
                        }
                    },
                    {
                        "id": "s2",
                        "name": "invoke_subagent",
                        "args": {
                            "Prompt": p_research,
                            "Role": "patch-reviewer",
                            "TypeName": "research",
                            "Workspace": "inherit",
                        }
                    },
                    {
                        "id": "s3",
                        "name": "invoke_subagent",
                        "args": {
                            "Prompt": p_unconfigured,
                            "Role": "Researcher",
                            "TypeName": "general",
                        }
                    }
                ])),
            ),
            subagent_created(4, CHILD_A, &[&uri(&wt2)]),
            subagent_created(5, CHILD_B, &[&uri(&fixture.repo)]),
            subagent_created(6, CHILD_C, &[&uri(&fixture.repo)]),
        ];
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        let long_pad = "z".repeat(12_500);
        let child_a_rows = vec![
            sys_msg_row(0, &msg_envelope(LEAD_ID, p_builder, true)),
            planner_row(
                1,
                Some(&format!("PREFIX_CLIPPED\n{long_pad}\nTAIL_OK\n{sample}")),
                None,
                None,
            ),
        ];
        let child_b_rows = vec![
            sys_msg_row(0, &msg_envelope(LEAD_ID, p_research, true)),
            planner_row(1, Some("RESEARCH_REPLY"), None, None),
        ];
        let child_c_rows = vec![
            sys_msg_row(0, &msg_envelope(LEAD_ID, p_unconfigured, true)),
            planner_row(1, Some("UNCONFIGURED_REPLY"), None, None),
        ];
        fixture.write_transcript(LEAD_ID, &lead_rows);
        fixture.write_transcript(CHILD_A, &child_a_rows);
        fixture.write_transcript(CHILD_B, &child_b_rows);
        fixture.write_transcript(CHILD_C, &child_c_rows);

        reset_all_agy_counters();
        let painted = BTreeSet::from([participant_id("antigravity", CHILD_A)]);
        let (_, locs) =
            agy_current_children(pid, &birth, &fixture.repo, Some(TURN_CUR), &painted).unwrap();
        assert_eq!(test_counters(), (2, 1, 0));
        assert_eq!(agy_child_read_counts(), (0, 0));
        assert_eq!(locs.len(), 1);

        reset_test_counters();
        let AgyChatOpen::OwnChat {
            history: builder_hist,
            locator: builder_loc,
        } = agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected OwnChat for builder child");
        };
        assert_eq!(test_counters(), (2, 1, 1));
        assert_eq!(agy_child_read_counts(), (0, 1));
        assert!(builder_hist.starts_with(
            "Recorded AGY setup (OS/shell access unverified):\nrole: builder · workspace: branch (linked worktree)\nrecorded grants: EnableWriteTools=true, EnableSubagentTools=false\n\n"
        ));
        assert!(builder_hist.contains("TAIL_OK"));
        assert!(
            builder_hist.contains("[oms-redacted:github-token]") && !builder_hist.contains(&sample)
        );
        assert!(!builder_hist.contains("PREFIX_CLIPPED"));
        for forbidden in [
            "read-only",
            "enforced",
            fixture.repo.to_str().unwrap(),
            wt2.to_str().unwrap(),
            LEAD_ID,
            CHILD_A,
            TURN_CUR,
        ] {
            let hdr = builder_hist.split("\n\n").next().unwrap();
            assert!(
                !hdr.to_ascii_lowercase()
                    .contains(&forbidden.to_ascii_lowercase())
            );
        }

        // agy_child_chat_fenced stays pure Phase B chat (no setup header).
        let fenced_hist =
            agy_child_chat_fenced(&fixture.repo, TURN_CUR, &builder_loc, || true).unwrap();
        assert!(!fenced_hist.contains("Recorded AGY setup"));
        assert!(fenced_hist.contains("TAIL_OK"));

        let sel_b = AgySelectedChild {
            pid,
            birth: birth.clone(),
            turn: TURN_CUR.into(),
            participant: participant_id("antigravity", CHILD_B),
        };
        let AgyChatOpen::OwnChat {
            history: research_hist,
            ..
        } = agy_open_selected_child_fenced(&fixture.repo, &sel_b, || true)
        else {
            panic!("expected OwnChat for research child");
        };
        assert_eq!(
            research_hist,
            "Recorded AGY setup (OS/shell access unverified):\nrole: patch-reviewer · workspace: inherit (shared parent checkout)\nrecorded grants: unknown\n\nRESEARCH_REPLY"
        );

        // Unconfigured invocation ("Researcher" non-allowlisted, no Workspace, no define_subagent)
        // omits the setup header completely and renders plain verified chat.
        let sel_c = AgySelectedChild {
            pid,
            birth,
            turn: TURN_CUR.into(),
            participant: participant_id("antigravity", CHILD_C),
        };
        reset_test_counters();
        let AgyChatOpen::OwnChat {
            history: unconfigured_hist,
            ..
        } = agy_open_selected_child_fenced(&fixture.repo, &sel_c, || true)
        else {
            panic!("expected OwnChat for unconfigured child");
        };
        assert_eq!(test_counters(), (2, 1, 1));
        assert_eq!(unconfigured_hist, "UNCONFIGURED_REPLY");

        // Live locator recheck passes cleanly with (2, 1, 0) initially.
        reset_test_counters();
        assert!(agy_locator_still_current(&fixture.repo, &builder_loc));
        assert_eq!(test_counters(), (2, 1, 0));

        // Appending a later sibling sharing wt2 preserves an open dock locator (0 child reads),
        // while a fresh explicit open reflects shared linked worktree isolation warning.
        let sibling_id = "99999999-9999-9999-9999-aaaaaaaaaaaa";
        let lead_path = fixture.transcript_path(LEAD_ID);
        let orig_lead = fs::read(&lead_path).unwrap();
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&to_jsonl(&[
                planner_row(
                    7,
                    None,
                    None,
                    Some(json!([
                        invoke_call(Some("s4"), "share wt2", "reviewer", "research"),
                        send_call("m1", sibling_id, "nudge"),
                    ])),
                ),
                subagent_created(8, sibling_id, &[&uri(&wt2)]),
                generic_row(9, &format!("Message sent to \"{sibling_id}\".")),
            ]))
            .unwrap();
        }
        reset_test_counters();
        assert!(agy_locator_still_current(&fixture.repo, &builder_loc));
        assert!(agy_locator_still_current_fenced(
            &fixture.repo,
            &builder_loc,
            || true
        ));
        assert!(agy_locator_still_current(&fixture.repo, &locs[0]));
        assert_eq!(test_counters(), (6, 3, 0));
        let AgyChatOpen::OwnChat {
            history: shared_reopen,
            ..
        } = agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
        else {
            panic!("expected OwnChat on explicit reopen with shared worktree sibling");
        };
        assert!(shared_reopen.starts_with(
            "Recorded AGY setup (OS/shell access unverified):\nrole: builder · workspace: branch (shared linked worktree; unverified isolation)\nrecorded grants: EnableWriteTools=true, EnableSubagentTools=false\n\n"
        ));

        // Restoring original lead and appending late/conflicting/failed define_subagent also
        // preserves an open dock locator (0 child reads), while explicit reopen degrades
        // recorded grants to unknown and retains ordinary verified chat.
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&lead_path)
                .unwrap();
            f.write_all(&orig_lead).unwrap();
        }
        for late_def in [
            vec![
                planner_row(
                    7,
                    None,
                    None,
                    Some(json!([{
                        "id": "d2",
                        "name": "define_subagent",
                        "args": {
                            "Name": "builder-worker",
                            "Description": "Redefined",
                            "SystemPrompt": "Still scoped.",
                            "EnableWriteTools": false,
                        }
                    }])),
                ),
                generic_row(
                    8,
                    "Subagent \"builder-worker\" defined successfully. It can now be invoked via invoke_subagent.",
                ),
            ],
            vec![
                planner_row(
                    7,
                    None,
                    None,
                    Some(json!([{
                        "id": "d3",
                        "name": "define_subagent",
                        "args": {
                            "Name": "builder-worker",
                            "Description": "Failed redef",
                            "SystemPrompt": "Still scoped.",
                        }
                    }])),
                ),
                json!({
                    "step_index": 8,
                    "source": "MODEL",
                    "type": "GENERIC",
                    "status": "ERROR",
                    "content": "define failed",
                }),
            ],
        ] {
            {
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&lead_path)
                    .unwrap();
                f.write_all(&orig_lead).unwrap();
            }
            {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&lead_path)
                    .unwrap();
                f.write_all(&to_jsonl(&late_def)).unwrap();
            }
            reset_test_counters();
            assert!(agy_locator_still_current(&fixture.repo, &builder_loc));
            assert!(agy_locator_still_current(&fixture.repo, &locs[0]));
            assert_eq!(test_counters(), (4, 2, 0));

            let AgyChatOpen::OwnChat {
                history: conflict_reopen,
                ..
            } = agy_open_selected_child_fenced(&fixture.repo, &sel_a, || true)
            else {
                panic!("expected OwnChat on explicit reopen after conflicting define_subagent");
            };
            assert!(conflict_reopen.starts_with(
                "Recorded AGY setup (OS/shell access unverified):\nrole: builder · workspace: branch (linked worktree)\nrecorded grants: unknown\n\n"
            ));
            assert!(conflict_reopen.contains("TAIL_OK"));
        }
    }
}
