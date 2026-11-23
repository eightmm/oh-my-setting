//! Bounded input delivery evidence for ordinary managed AGY and Claude leads.
use super::{AccountHost, CHAT_SGR, PaneIdentity, Panel, assignment_shell_word, input_ready};
use crate::{Error, Result, require};
use oms_panel_core::{Store, Transaction, services::ExecutionRequest, terminals::SessionProof};
use oms_safe_fs::{ByteReadOptions, MutationError, MutationPhase, Root};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_RECEIPT: usize = 8 * 1024;
const MAX_TICKET: usize = 64 * 1024;
const GROUP_PROOF_BUDGET: Duration = Duration::from_secs(2);
const MAX_FRAME: usize = 64 * 1024;
const MAX_ROWS: usize = 160;
pub(super) const TICKET_SERVER: &str = "#{pid}\t#{start_time}\t#{session_created}\t.";
pub(super) const ENTER_SENT: &str = "OMS_AGY_ENTER_SENT";
pub(super) const ENTER_REFUSED: &str = "OMS_AGY_ENTER_REFUSED";
pub(super) const TICKET_CWD: &str = "#{pane_current_path}\t.";
pub(super) const FRAME: &str = concat!(
    "#{pane_id}\t#{pane_dead}\t#{pane_in_mode}\t#{cursor_x}\t#{cursor_y}\t",
    "#{pane_width}\t#{pane_height}\t.",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Delivery {
    Unverified,
    DraftRetained,
    InputClearedUnverified,
    TurnObserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) delivery: Delivery,
    pub(super) reason: Option<&'static str>,
}

impl Outcome {
    pub(super) fn uncertain(reason: &'static str) -> Self {
        Self {
            delivery: Delivery::Unverified,
            reason: Some(reason),
        }
    }

    pub(super) fn reconcile_cue(self) -> &'static str {
        self.reason.unwrap_or(match self.delivery {
            Delivery::DraftRetained => "The draft is still visible. Inspect the provider pane before resending.",
            Delivery::InputClearedUnverified => "Input cleared without verified provider acceptance. Inspect the provider pane before resending.",
            Delivery::Unverified => "Provider acceptance could not be verified. Inspect the provider pane before resending.",
            Delivery::TurnObserved => "The turn was observed but its receipt could not be updated. Inspect the provider pane before resending.",
        })
    }
}

impl From<Delivery> for Outcome {
    fn from(delivery: Delivery) -> Self {
        Self {
            delivery,
            reason: None,
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    session: SessionProof,
    room: String,
    attempt: String,
    member: String,
    provider: String,
    main: PaneIdentity,
    lead: PaneIdentity,
}

impl Scope {
    pub(super) fn sample(
        panel: &Panel<'_>,
        provider: &str,
        host: &dyn AccountHost,
    ) -> Result<Self> {
        let lead = panel
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("provider pane unavailable"))?;
        Ok(Self {
            session: panel.session_proof.clone(),
            room: panel.room.into(),
            attempt: panel.attempt.clone(),
            member: panel.participant.clone(),
            provider: provider.into(),
            main: PaneIdentity::sample_with_host(&panel.main, host, true)?,
            lead: PaneIdentity::sample_with_host(lead, host, true)?,
        })
    }

    pub(super) fn proves_agy_turn(&self, repo: &Path, turn: &str, prompt: &str) -> bool {
        self.provider == "antigravity"
            && crate::native_children::agy_submitted_user_turn(
                self.lead.pid,
                &self.lead.birth,
                repo,
                turn,
                prompt,
            )
    }
}

/// An ordinary AGY turn carries a reference. A native user-input record can
/// confirm the reference was submitted, while reading its body remains unverified.
pub(super) struct AgyTicket<'a> {
    pub(super) prompt: String,
    project: Root,
    path: PathBuf,
    bytes: Vec<u8>,
    generation: PrivateFileGeneration,
    dimensions: (usize, usize),
    server_times: (u64, u64),
    room_log: PathBuf,
    room_generation: RefCell<Option<RoomGeneration>>,
    tx: &'a Transaction<'a>,
}

impl<'a> AgyTicket<'a> {
    pub(super) fn prepare(
        panel: &Panel<'_>,
        scope: &Scope,
        tx: &'a Transaction<'a>,
        turn: &str,
        payload: &str,
    ) -> Result<Self> {
        require(
            scope.provider == "antigravity" && valid_turn(turn),
            "AGY task reference scope is invalid",
        )?;
        let canonical = panel.repo.canonicalize().map_err(Error::new)?;
        let project = Root::open_private_workspace(canonical, group_check(panel))
            .map_err(|_| Error::new("AGY task reference workspace ancestry is unsafe"))?;
        require_room_repo(
            project.physical_root(),
            std::env::var_os("OMS_STATE_REPO").as_deref(),
        )?;
        Self::require_cwd(panel, scope, &project)?;
        let dimensions = Self::dimensions(panel, scope)?;
        let (width, height) = dimensions;
        let server_times = Self::server_times(panel, scope)?;
        let repo = project.physical_root().to_str().ok_or_else(|| {
            Error::new("repository cannot be represented in an AGY task reference")
        })?;
        require(
            safe_guard_atom(&scope.room),
            "AGY task reference Room identity is invalid",
        )?;
        let room_log = project
            .physical_root()
            .join(format!(".oms/threads/{}.jsonl", scope.room));
        let locator = format!(".oms/artifacts/native-agy-tasks/{turn}.json");
        let path = PathBuf::from(&locator);
        let prompt = format!(
            "{} Turn {turn}. You own this provider's work. Read {}; stop if unavailable.",
            crate::native_children::TASK_PREFIX,
            assignment_shell_word(&locator),
        );
        // Account for the prompt marker, cursor row, and both composer rules.
        // The actual capture must still prove every byte, including any wraps.
        let rows = (crate::render::width(&prompt) + 2) / width + 3;
        require(
            prompt.is_ascii() && rows <= height,
            "AGY task reference cannot fit the verified composer viewport",
        )?;
        let packet = json!({
            "schema": 1, "kind": "oms-ordinary-agy-task", "turn": turn,
            "repo": repo, "main_owner": panel.owner, "scope": scope,
            "payload": payload, "payload_sha256": oms_panel_core::digest(payload.as_bytes()),
        });
        let bytes = serde_json::to_vec(&packet).map_err(Error::new)?;
        for data in [payload.as_bytes(), bytes.as_slice()] {
            require(
                oms_panel_core::redact(data)?.as_bytes() == data,
                "AGY task reference content refused by scrubber",
            )?;
        }
        tx.check()?;
        project
            .create_private(&path, &bytes, MAX_TICKET)
            .map_err(|error| match error {
                MutationError::Storage { phase, .. } => Error {
                    publication_uncertain: phase != MutationPhase::BeforePublication,
                    ..Error::new("AGY task reference could not be created exclusively")
                },
                MutationError::Transform(never) => match never {},
            })?;
        let generation = PrivateFileGeneration::verified(&project, &path, &bytes)?;
        Ok(Self {
            prompt,
            project,
            path,
            bytes,
            generation,
            dimensions,
            server_times,
            room_log,
            room_generation: RefCell::default(),
            tx,
        })
    }

    fn server_times(panel: &Panel<'_>, scope: &Scope) -> Result<(u64, u64)> {
        const INVALID: &str = "AGY task reference tmux generation unavailable";
        let raw = panel.tmux(&["display-message", "-p", "-t", &scope.lead.id, TICKET_SERVER])?;
        let fields: Vec<_> = raw.split('\t').collect();
        let [pid, start, created, "."] = fields.as_slice() else {
            return Err(Error::new(INVALID));
        };
        let start = start.parse().map_err(|_| Error::new(INVALID))?;
        let created = created.parse().map_err(|_| Error::new(INVALID))?;
        require(
            pid.parse() == Ok(scope.session.server_pid) && start > 0 && created >= start,
            INVALID,
        )?;
        Ok((start, created))
    }

    pub(super) fn verify_room(&self, verify: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
        self.room_generation.replace(None);
        let state_repo = std::env::var_os("OMS_STATE_REPO");
        let generation =
            verify_room_generation(&self.project, &self.room_log, state_repo.as_deref(), verify)?;
        self.room_generation.replace(Some(generation));
        Ok(())
    }

    pub(super) fn enter(
        &self,
        scope: &Scope,
        draft: &Draft,
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
    ) -> Result<bool> {
        let room = RoomGeneration::sample(
            &self.project,
            &self.room_log,
            std::env::var_os("OMS_STATE_REPO").as_deref(),
        )
        .ok();
        if room.is_none() || room != *self.room_generation.borrow() {
            return Ok(false);
        }
        send_guarded_enter(
            tmux,
            self.project.physical_root(),
            scope,
            self.server_times,
            draft.cursor,
        )
    }

    fn require_cwd(panel: &Panel<'_>, scope: &Scope, root: &Root) -> Result<()> {
        const INVALID: &str = "AGY lead cwd is not the physical repository root";
        let error = || Error::new(INVALID);
        let mut request = ExecutionRequest::new(
            [
                "tmux",
                "display-message",
                "-p",
                "-t",
                &scope.lead.id,
                TICKET_CWD,
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            panel.repo,
        );
        request.timeout = Duration::from_secs(3);
        request.output_limit = 4098;
        let output = panel
            .services
            .execute(&request, panel.stop)
            .map_err(|_| error())?;
        require(
            output.code == 0
                && output.cleanup_complete
                && output.output_complete
                && !output.limited,
            INVALID,
        )?;
        // The display adapter normally scrubs terminal controls. A path proof
        // must preserve them and refuse the path if any are present.
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| error())?;
        let cwd = raw
            .strip_suffix('\n')
            .unwrap_or(raw)
            .strip_suffix("\t.")
            .filter(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
            .map(Path::new);
        require(cwd == Some(root.physical_root()), INVALID)?;
        let (held, _) = root
            .pinned_metadata(OsStr::new(".oms"))
            .map_err(|_| error())?;
        let named = rustix::fs::lstat(root.physical_root()).map_err(|_| error())?;
        require(
            rustix::fs::FileType::from_raw_mode(named.st_mode) == rustix::fs::FileType::Directory
                && (named.st_dev, named.st_ino) == (held.st_dev, held.st_ino),
            INVALID,
        )
    }

    fn dimensions(panel: &Panel<'_>, scope: &Scope) -> Result<(usize, usize)> {
        let pane = &scope.lead.id;
        let metadata = panel.tmux(&["display-message", "-p", "-t", pane, FRAME])?;
        let cursor = Cursor::parse(&metadata, pane)?;
        Ok((cursor.width, cursor.height))
    }

    pub(super) fn revalidate(&self, panel: &Panel<'_>, scope: &Scope) -> Result<()> {
        self.tx.check()?;
        self.project
            .recheck_private_workspace(group_check(panel))
            .map_err(|_| Error::new("AGY task reference workspace ancestry changed"))?;
        Self::require_cwd(panel, scope, &self.project)?;
        require(
            Self::dimensions(panel, scope)? == self.dimensions,
            "AGY task reference viewport changed",
        )?;
        require(
            PrivateFileGeneration::verified(&self.project, &self.path, &self.bytes)?
                == self.generation,
            "AGY task reference is missing or changed",
        )
    }
}

fn require_room_repo(project: &Path, state_repo: Option<&OsStr>) -> Result<()> {
    require(
        state_repo
            .filter(|path| !path.is_empty())
            .is_none_or(|state| state == project.as_os_str()),
        "AGY task reference requires Room state in the physical project repository",
    )
}

fn verify_room_generation(
    project: &Root,
    path: &Path,
    state_repo: Option<&OsStr>,
    verify: impl FnOnce(&Path) -> Result<()>,
) -> Result<RoomGeneration> {
    let before = RoomGeneration::sample(project, path, state_repo)?;
    verify(project.physical_root())?;
    require(
        RoomGeneration::sample(project, path, state_repo)? == before,
        "AGY task reference Room changed during verification",
    )?;
    Ok(before)
}

fn tmux_format_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '#' | ',' | '}') {
            literal.push('#');
        }
        literal.push(character);
    }
    literal
}

fn send_guarded_enter(
    tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
    project: &Path,
    scope: &Scope,
    (server_start, session_created): (u64, u64),
    Cursor {
        x,
        row,
        width,
        height,
    }: Cursor,
) -> Result<bool> {
    let path = project
        .to_str()
        .filter(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
        .ok_or_else(|| Error::new("AGY task reference workspace path is unrepresentable"))?;
    let expected_path = tmux_format_literal(path);
    let wanted = format!(
        "{}:{server_start}:{session_created}:{}:{}:{}:{}:0:0:0:{}:{}:{}:{x}:{row}:{width}:{height}",
        scope.session.server_pid,
        scope.session.session,
        scope.lead.window,
        scope.lead.id,
        scope.lead.pid,
        scope.room,
        scope.provider,
        scope.attempt,
    );
    let predicate = format!(
        "#{{&&:#{{&&:#{{==:#{{pane_current_path}},{expected_path}}},#{{==:#{{@oms_panel_repo}},{expected_path}}}}},#{{==:#{{pid}}:#{{start_time}}:#{{session_created}}:#{{session_id}}:#{{window_id}}:#{{pane_id}}:#{{pane_pid}}:#{{pane_dead}}:#{{&&:#{{window_active}},#{{pane_active}}}}:#{{pane_in_mode}}:#{{@oms_panel_room}}:#{{@oms_provider_session}}:#{{@oms_provider_parent}}:#{{cursor_x}}:#{{cursor_y}}:#{{pane_width}}:#{{pane_height}},{wanted}}}}}"
    );
    let yes = format!(
        "send-keys -t {} Enter ; display-message -p {ENTER_SENT}",
        assignment_shell_word(&scope.lead.id),
    );
    let no = format!("display-message -p {ENTER_REFUSED}");
    match tmux(&[
        "if-shell",
        "-F",
        "-t",
        &scope.lead.id,
        &predicate,
        &yes,
        &no,
    ])?
    .as_str()
    {
        ENTER_SENT => Ok(true),
        ENTER_REFUSED => Ok(false),
        _ => Err(Error::new("AGY Enter delivery is uncertain")),
    }
}

fn group_check<'a>(panel: &'a Panel<'_>) -> impl FnMut(u32) -> bool + 'a {
    let deadline = Instant::now() + GROUP_PROOF_BUDGET;
    let uid = rustix::process::getuid().as_raw();
    move |gid| {
        Instant::now() < deadline
            && panel
                .services
                .workspace_group(gid, deadline, panel.stop)
                .is_some_and(|proof| {
                    Instant::now() < deadline
                        && proof.gid == gid
                        && proof
                            .explicit_member_uids
                            .iter()
                            .all(|member| *member == uid)
                        && proof.primary_member_uids == [uid]
                })
    }
}

fn safe_guard_atom(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

#[derive(PartialEq, Eq)]
struct RoomGeneration {
    oms: (rustix::fs::Dev, u64),
    threads: (u64, u64),
    log: PrivateFileGeneration,
}

impl RoomGeneration {
    fn sample(project: &Root, path: &Path, state_repo: Option<&OsStr>) -> Result<Self> {
        const INVALID: &str = "AGY task reference Room generation unavailable";
        require_room_repo(project.physical_root(), state_repo)?;
        let uid = rustix::process::geteuid().as_raw();
        let directories = || -> Result<_> {
            let (_, oms) = project
                .pinned_metadata(OsStr::new(".oms"))
                .map_err(|_| Error::new(INVALID))?;
            let oms = oms
                .filter(|meta| {
                    rustix::fs::FileType::from_raw_mode(meta.st_mode)
                        == rustix::fs::FileType::Directory
                        && meta.st_uid == uid
                        && meta.st_mode & 0o022 == 0
                })
                .ok_or_else(|| Error::new(INVALID))?;
            let threads = path.parent().ok_or_else(|| Error::new(INVALID))?;
            let threads = std::fs::symlink_metadata(threads).map_err(|_| Error::new(INVALID))?;
            require(
                threads.is_dir() && threads.uid() == uid && threads.mode() & 0o022 == 0,
                INVALID,
            )?;
            Ok(((oms.st_dev, oms.st_ino), (threads.dev(), threads.ino())))
        };
        let (oms, threads) = directories()?;
        let log = std::fs::symlink_metadata(path).map_err(|_| Error::new(INVALID))?;
        require(
            log.is_file() && log.nlink() == 1 && log.uid() == uid && log.mode() & 0o022 == 0,
            INVALID,
        )?;
        require(directories()? == (oms, threads), INVALID)?;
        Ok(Self {
            oms,
            threads,
            log: PrivateFileGeneration::from_meta(&log),
        })
    }
}

#[derive(PartialEq, Eq)]
struct PrivateFileGeneration {
    inode: (u64, u64),
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl PrivateFileGeneration {
    fn from_meta(meta: &std::fs::Metadata) -> Self {
        Self {
            inode: (meta.dev(), meta.ino()),
            bytes: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }

    fn verified(root: &Root, path: &Path, expected: &[u8]) -> Result<Self> {
        const INVALID: &str = "AGY task reference is missing or changed";
        let named = root.physical_root().join(path);
        let identity = || -> Result<Self> {
            let meta = std::fs::symlink_metadata(&named).map_err(|_| Error::new(INVALID))?;
            require(
                meta.is_file()
                    && meta.uid() == rustix::process::getuid().as_raw()
                    && meta.mode() & 0o7777 == 0o600
                    && meta.nlink() == 1
                    && meta.len() == expected.len() as u64,
                INVALID,
            )?;
            Ok(Self::from_meta(&meta))
        };
        let before = identity()?;
        let options = ByteReadOptions {
            max_bytes: MAX_TICKET,
            missing_ok: false,
        };
        let bytes = root
            .read_no_follow(path, options)
            .map_err(|_| Error::new(INVALID))?;
        require(bytes == expected && identity()? == before, INVALID)?;
        Ok(before)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub(super) scope: Scope,
    turn_id: String,
    pub(super) delivery: Delivery,
}

impl Receipt {
    pub(super) fn new(scope: Scope, turn: &str) -> Self {
        Self {
            scope,
            turn_id: turn.into(),
            delivery: Delivery::Unverified,
        }
    }

    pub(super) fn write(&self, tx: &Transaction<'_>, provider: &str) -> Result<()> {
        let path =
            receipt_path(provider).ok_or_else(|| Error::new("invalid lead receipt provider"))?;
        tx.write_json(
            &path,
            &serde_json::to_value(self).map_err(Error::new)?,
            false,
            MAX_RECEIPT,
        )?;
        Ok(())
    }

    pub(super) fn for_live_lead(
        store: &Store,
        panel: &Panel<'_>,
        provider: &str,
        host: &dyn AccountHost,
    ) -> Option<Delivery> {
        let raw = store
            .json(&receipt_path(provider)?, MAX_RECEIPT, true)
            .ok()??;
        let receipt: Self = serde_json::from_value(raw).ok()?;
        (receipt.scope == Scope::sample(panel, provider, host).ok()?
            && valid_turn(&receipt.turn_id))
        .then_some(receipt.delivery)
    }
}

fn receipt_path(provider: &str) -> Option<PathBuf> {
    matches!(provider, "claude" | "antigravity")
        .then(|| format!(".oms/artifacts/native-lead-delivery/{provider}.json").into())
}

fn valid_turn(turn: &str) -> bool {
    turn.len() == 20
        && turn
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    x: usize,
    row: usize,
    width: usize,
    height: usize,
}

impl Cursor {
    fn parse(metadata: &str, pane: &str) -> Result<Self> {
        let fields: Vec<_> = metadata.split('\t').collect();
        let [found, "0", "0", x, row, width, height, "."] = fields.as_slice() else {
            return Err(Error::new("lead delivery viewport changed"));
        };
        let cursor = Self {
            x: x.parse().map_err(Error::new)?,
            row: row.parse().map_err(Error::new)?,
            width: width.parse().map_err(Error::new)?,
            height: height.parse().map_err(Error::new)?,
        };
        require(
            *found == pane
                && cursor.height <= MAX_ROWS
                && cursor.row < cursor.height
                && cursor.x < cursor.width,
            "lead delivery viewport changed",
        )?;
        Ok(cursor)
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct Draft {
    cursor: Cursor,
    start: usize,
    rows: Vec<String>,
}

pub(super) struct Frame {
    rows: Vec<String>,
    cursor: Cursor,
}

impl Frame {
    fn read(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
        capture: &[&str],
    ) -> Result<(Cursor, String)> {
        let mut argv = vec![
            "display-message",
            "-p",
            "-t",
            pane,
            FRAME,
            ";",
            "capture-pane",
            "-p",
            "-e",
        ];
        argv.extend_from_slice(capture);
        // Keep trailing input spaces intact through the tmux adapter's stdout trim.
        argv.extend_from_slice(&["-t", pane, ";", "display-message", "-p", "-t", pane, "."]);
        let output = tmux(&argv)?;
        require(
            output.len() <= MAX_FRAME,
            "lead delivery viewport too large",
        )?;
        let (head, screen) = output
            .split_once('\n')
            .and_then(|(head, body)| Some((head, body.strip_suffix("\n.")?)))
            .ok_or_else(|| Error::new("lead delivery viewport unavailable"))?;
        Ok((Cursor::parse(head, pane)?, screen.to_owned()))
    }

    pub(super) fn capture(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
    ) -> Result<Self> {
        let (cursor, screen) = Self::read(tmux, pane, &["-S", "0"])?;
        let rows: Vec<_> = screen.lines().map(str::to_owned).collect();
        require(
            rows.len() <= cursor.height && cursor.row < rows.len(),
            "lead delivery viewport changed",
        )?;
        Ok(Self { rows, cursor })
    }

    pub(super) fn exact_draft(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
        provider: &str,
        expected: &str,
    ) -> Option<Draft> {
        let frame = Self::capture(tmux, pane).ok()?;
        let draft = frame.draft(provider)?;
        let cursor = draft.cursor;
        // One logical line, using tmux's wrap metadata and preserving even
        // single-row trailing spaces. App-painted rows can hide separators.
        let start = draft.start.to_string();
        let end = cursor.row.to_string();
        let (checked, joined) = Self::read(tmux, pane, &["-J", "-S", &start, "-E", &end]).ok()?;
        if checked != cursor {
            return None;
        }
        let line = CHAT_SGR.replace_all(&joined, "");
        let cells = crate::render::width(&line);
        (!line.contains(['\r', '\n'])
            && !expected.is_empty()
            && composer_body(provider, &line) == Some(expected)
            && cells / cursor.width == cursor.row - draft.start
            && cells % cursor.width == cursor.x)
            .then_some(draft)
    }

    fn draft(&self, provider: &str) -> Option<Draft> {
        let rows = &self.rows[..=self.cursor.row];
        let start = rows
            .iter()
            .rposition(|line| composer_body(provider, &CHAT_SGR.replace_all(line, "")).is_some())?;
        let top = CHAT_SGR.replace_all(self.rows.get(start.checked_sub(1)?)?, "");
        let bottom = CHAT_SGR.replace_all(self.rows.get(self.cursor.row + 1)?, "");
        // The two provider composer rules distinguish the last editable row
        // from an earlier cursor position with unverified input below it.
        let rule = top.trim_end_matches(' ');
        if rule != bottom.trim_end_matches(' ')
            || rule.chars().count() < 3
            || !rule.chars().all(|cell| cell == '─')
        {
            return None;
        }
        Some(Draft {
            cursor: self.cursor,
            start,
            rows: rows[start..].to_vec(),
        })
    }

    pub(super) fn shows_turn(&self, turn: &str) -> bool {
        self.rows
            .iter()
            .any(|line| CHAT_SGR.replace_all(line, "").contains(turn))
    }

    pub(super) fn delivery(&self, provider: &str, prior_draft: Option<&Draft>) -> Delivery {
        if self.empty(provider) {
            return Delivery::InputClearedUnverified;
        }
        if prior_draft.is_some() && self.draft(provider).as_ref() == prior_draft {
            return Delivery::DraftRetained;
        }
        Delivery::Unverified
    }

    fn empty(&self, provider: &str) -> bool {
        let line = CHAT_SGR.replace_all(&self.rows[self.cursor.row], "");
        self.cursor.x == 2
            && (composer_body(provider, &line).is_some()
                || matches!(
                    (provider, line.as_ref()),
                    ("antigravity", ">") | ("claude", "❯")
                ))
            && input_ready(provider, &self.rows[self.cursor.row..].join("\n"))
    }
}

fn composer_body<'a>(provider: &str, line: &'a str) -> Option<&'a str> {
    match provider {
        "antigravity" => line.strip_prefix("> "),
        "claude" => line
            .strip_prefix("❯ ")
            .or_else(|| line.strip_prefix("❯\u{a0}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{SystemAccountHost, codex_chat_fixture::BoardSpy};

    #[test]
    fn ordinary_agy_ticket_refuses_split_room_state() {
        use super::super::account_binding::test_support::{TempDir, private_dir};
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new("agy-state");
        let project = private_dir(&temp.0.join("project"));
        let other = private_dir(&temp.0.join("other"));
        let alias = temp.0.join("alias");
        symlink(&project, &alias).unwrap();
        assert!(require_room_repo(&project, None).is_ok());
        assert!(require_room_repo(&project, Some(project.as_os_str())).is_ok());
        for state in [
            alias,
            other,
            temp.0.join("missing"),
            project.join("."),
            temp.0.join("project/../project"),
            PathBuf::from("project"),
        ] {
            assert!(require_room_repo(&project, Some(state.as_os_str())).is_err());
        }

        let oms = private_dir(&project.join(".oms"));
        let threads = private_dir(&oms.join("threads"));
        let room = threads.join("room-fixture.jsonl");
        std::fs::write(&room, b"room fixture").unwrap();
        std::fs::set_permissions(&room, std::fs::Permissions::from_mode(0o600)).unwrap();
        let root = Root::open_private_workspace(&project, |_| false).unwrap();
        let explicit = Some(project.as_os_str());
        assert!(
            verify_room_generation(&root, &room, explicit, |repo| {
                assert_eq!(repo.as_os_str(), project.as_os_str());
                Ok(())
            })
            .is_ok()
        );
        assert!(
            verify_room_generation(&root, &room, explicit, |_| {
                std::fs::write(&room, b"changed room fixture").unwrap();
                Ok(())
            })
            .is_err()
        );
        let moved = temp.0.join("moved");
        let inode = std::fs::symlink_metadata(&room).unwrap().ino();
        assert!(
            verify_room_generation(&root, &room, explicit, |_| {
                std::fs::rename(&project, &moved).unwrap();
                symlink(&moved, &project).unwrap();
                assert_eq!(std::fs::symlink_metadata(&room).unwrap().ino(), inode);
                Ok(())
            })
            .is_err()
        );
        assert!(RoomGeneration::sample(&root, &room, explicit).is_err());
    }

    #[test]
    fn ordinary_agy_enter_pins_exact_path_in_tmux_server() {
        use super::super::account_binding::test_support::{TempDir, private_dir};
        use std::process::Command;

        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return;
        }
        struct Server(PathBuf);
        impl Server {
            fn run(&self, args: &[&str]) -> Result<String> {
                let output = Command::new("tmux")
                    .args(["-u", "-f", "/dev/null", "-S"])
                    .arg(&self.0)
                    .args(args)
                    .env_remove("TMUX")
                    .env_remove("TMUX_PANE")
                    .output()
                    .map_err(Error::new)?;
                require(
                    output.status.success(),
                    &String::from_utf8_lossy(&output.stderr),
                )?;
                Ok(String::from_utf8(output.stdout)
                    .map_err(Error::new)?
                    .trim_end_matches('\n')
                    .to_owned())
            }
        }
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.run(&["kill-server"]);
            }
        }
        fn until(mut matches: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !matches() {
                assert!(
                    Instant::now() < deadline,
                    "tmux fixture did not reach its expected state"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        let temp = TempDir::new("agy-enter");
        let repo = private_dir(&temp.0.join("répo#,{x}#{pane_pid},ré}"));
        let other = private_dir(&temp.0.join("other"));
        let count = temp.0.join("keys");
        let server = Server(temp.0.join("tmux"));
        let run = |args: &[&str]| server.run(args).unwrap();
        run(&[
            "new-session",
            "-d",
            "-s",
            "fixture",
            "-x",
            "100",
            "-y",
            "25",
            "-c",
            &tmux_format_literal(repo.to_str().unwrap()),
            "/bin/sh",
            "-c",
            r#"stty -echo
printf ready > "$1"
while IFS= read -r directory; do
    if [ -n "$directory" ]; then cd -P -- "$directory" || exit
    else printf x >> "$1"
    fi
done"#,
            "sh",
            count.to_str().unwrap(),
        ]);
        until(|| std::fs::read(&count).is_ok_and(|bytes| bytes == b"ready"));
        run(&[
            "new-window",
            "-t",
            "fixture",
            "-n",
            "control",
            "exec /bin/sleep 60",
        ]);
        let target = "fixture:0.0";
        let display = |format| run(&["display-message", "-p", "-t", target, format]);
        assert_eq!(display("#{&&:#{window_active},#{pane_active}}"), "0");
        let set = |key, value| {
            run(&["set-option", "-w", "-t", target, key, value]);
        };
        let info = display(
            "#{pid}\t#{start_time}\t#{session_created}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}",
        );
        let fields: Vec<_> = info.split('\t').collect();
        let [
            server_pid,
            start,
            created,
            session,
            window,
            pane_id,
            pane_pid,
        ] = fields.as_slice()
        else {
            panic!("missing tmux fixture generation: {info}");
        };
        let pane = PaneIdentity {
            window: (*window).into(),
            id: (*pane_id).into(),
            pid: pane_pid.parse().unwrap(),
            birth: "fixture".into(),
        };
        let scope = Scope {
            session: SessionProof {
                session: (*session).into(),
                server_pid: server_pid.parse().unwrap(),
                server_birth: "fixture".into(),
                socket_digest: String::new(),
            },
            room: "room-fixture".into(),
            attempt: "attempt-fixture".into(),
            member: "member-fixture".into(),
            provider: "antigravity".into(),
            main: pane.clone(),
            lead: pane,
        };
        for (key, value) in [
            ("@oms_panel_room", scope.room.as_str()),
            ("@oms_provider_session", scope.provider.as_str()),
            ("@oms_provider_parent", scope.attempt.as_str()),
        ] {
            set(key, value);
        }
        let frame = display(FRAME);
        let cursor = Cursor::parse(&frame, pane_id).unwrap();
        let guarded_enter = || {
            send_guarded_enter(
                &mut |args| server.run(args),
                &repo,
                &scope,
                (start.parse().unwrap(), created.parse().unwrap()),
                cursor,
            )
            .unwrap()
        };
        let cwd = |path: &Path| {
            run(&["send-keys", "-t", target, "-l", path.to_str().unwrap()]);
            run(&["send-keys", "-t", target, "Enter"]);
            until(|| Path::new(&display("#{pane_current_path}")) == path);
            assert_eq!(display(FRAME), frame);
        };

        assert_eq!(Path::new(&display("#{pane_current_path}")), repo);
        set("@oms_panel_repo", other.to_str().unwrap());
        assert!(
            !guarded_enter(),
            "changed repo tag alone must withhold Enter"
        );
        cwd(&other);
        assert!(
            !guarded_enter(),
            "matching live CWD and tag cannot substitute a different repo"
        );
        set("@oms_panel_repo", repo.to_str().unwrap());
        assert!(!guarded_enter(), "changed CWD alone must withhold Enter");
        assert_eq!(std::fs::read(&count).unwrap(), b"ready");
        cwd(&repo);
        assert!(guarded_enter(), "exact escaped repository must send");
        until(|| std::fs::read(&count).is_ok_and(|bytes| bytes == b"readyx"));
    }

    #[test]
    fn ordinary_lead_delivery_receipt_requires_live_generation() {
        let observed = BoardSpy::new("delivery");
        observed.provider.set("antigravity");
        let panel = observed.panel();
        let store = Store::open(panel.repo).unwrap();
        let tx = store
            .transaction("panel-provider-sessions", std::time::Duration::from_secs(1))
            .unwrap();
        let host = &SystemAccountHost;
        let mut receipt = Receipt::new(
            Scope::sample(&panel, "antigravity", host).unwrap(),
            "a0123b0123c0123d0123",
        );
        receipt.write(&tx, "antigravity").unwrap();
        assert_eq!(
            Receipt::for_live_lead(&store, &panel, "antigravity", host),
            Some(Delivery::Unverified),
        );
        receipt.delivery = Delivery::TurnObserved;
        receipt.write(&tx, "antigravity").unwrap();
        assert_eq!(
            Receipt::for_live_lead(&store, &panel, "antigravity", host),
            Some(Delivery::TurnObserved),
        );
        observed.lead.set("%4");
        assert!(Receipt::for_live_lead(&store, &observed.panel(), "antigravity", host).is_none());
        observed.lead.set("%3");
        receipt.scope.lead.birth = "stale-process".into();
        receipt.write(&tx, "antigravity").unwrap();
        assert!(Receipt::for_live_lead(&store, &panel, "antigravity", host).is_none());
        receipt.scope = Scope::sample(&panel, "antigravity", host).unwrap();
        receipt.scope.room = "room-foreign".into();
        receipt.write(&tx, "antigravity").unwrap();
        assert!(Receipt::for_live_lead(&store, &panel, "antigravity", host).is_none());
    }
}
