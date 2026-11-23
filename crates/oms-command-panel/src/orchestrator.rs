//! Persistent native provider windows beneath the room's interactive main.
#[path = "account_binding.rs"]
pub(crate) mod account_binding;
#[path = "lead_delivery.rs"]
mod lead_delivery;
#[path = "native_assignment.rs"]
mod native_assignment;
#[path = "native_result.rs"]
mod native_result;
#[path = "native_succession.rs"]
mod native_succession;

use crate::claude_lead_usage::LeadReading;
use crate::native_panel::{MAIN_PERMISSIONS_REF, MainPermissions};
use crate::{Error, Output, Result, Services, cli::Options, commands, json, require, text};
use account_binding::{
    AccountHost, AccountSelectionReceipt, AccountVerification, AlternateLeadScope, BindingAnchor,
    CodexChatHome, PersistedAccountBinding, SelectionTarget, SystemAccountHost,
};
use oms_command_peer::native_model::{self, AGY_FIXED_EFFORT_MODELS};
use oms_panel_core::native::reference::process::Owner;
use oms_panel_core::{NativeCommand, Store, Transaction, Value, state::AttemptCache, terminals};
use serde_json::json;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

enum LeadAccount<'a> {
    Default(Option<&'a str>),
    Spread(&'a Value),
}

struct LeadExecution<'a> {
    host: &'a dyn AccountHost,
    attempts: &'a AttemptCache,
    transaction: Option<&'a Transaction<'a>>,
}

#[derive(Clone, Copy)]
enum AssignmentEvidence<'a> {
    Bound(&'a native_assignment::Binding),
    Primary(&'a Transaction<'a>),
    Lead(
        &'a lead_delivery::Scope,
        &'a dyn AccountHost,
        &'a AttemptCache,
    ),
}

#[derive(Clone, Copy, Default)]
struct LeadLaunch<'a> {
    model: Option<&'a str>,
    effort: Option<&'a str>,
    permissions: Option<MainPermissions>,
}

impl<'a> LeadLaunch<'a> {
    fn model(self, provider: &str) -> Result<&'a str> {
        let default = orchestrator_default_model(provider)?;
        let requested = self.model.unwrap_or(default);
        if provider == "antigravity"
            && let Some(fixed) = agy_fixed_effort_model(requested)
        {
            return Ok(fixed);
        }
        Ok(requested)
    }

    fn explicit(self) -> bool {
        self.model.is_some() || self.effort.is_some()
    }
}

pub const PROVIDERS: [&str; 3] = ["codex", "claude", "antigravity"];
pub const MAIN_WINDOW: &str = "Overall Main";

const STARTUP_PROMPT: &str = include_str!("provider-coordination.txt");
const REQUESTED_PROVIDER_MODEL: &str = "@oms_provider_requested_model";
const ALTERNATE_LEAD_TAG: &str = "codex-alternate";
const ALTERNATE_TASK_INPUT_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{window_panes}\t.\t#{window_active}\t#{pane_in_mode}\t#{cursor_x}\t#{cursor_y}\t#{pane_current_command}\t.";
const CODEX_VISIBLE_PASTE_CHARS: usize = 1000;
const ALTERNATE_STARTUP_PROMPT: &str = "oms list; oms agent-role show --name ROLE. Delegate substantive work to native children. Make only necessary integration or unblocking edits yourself. Role/disjoint scope/checks; native read-only research/review/verify if available; builders edit assigned paths. No child delegate/commit/push/land/install/publish; parent to Main. Simple bounded: cheaper supported native model, escalate on scope/risk, keep user choice/account/isolation/spend. Verify; report evidence/blockers/unverified access/actual model/unknown.";
const PROVIDER_SPAWN_FORMAT: &str = "#{pane_id}\t#{pane_pid}";
const ALTERNATE_SPAWN_FORMAT: &str = "#{pane_id}\t#{pane_pid}\t#{window_id}\t#{session_id}";
const ALTERNATE_PANE_FORMAT: &str =
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{window_panes}\t.";

pub(crate) fn agy_fixed_effort_model(raw: &str) -> Option<&'static str> {
    native_model::fixed_effort_model("antigravity", raw).or_else(|| {
        let canonical = crate::native_panel::normalize_antigravity_model(raw)?;
        native_model::fixed_effort_model("antigravity", &canonical)
    })
}

pub(crate) fn orchestrator_default_model(provider: &str) -> Result<&'static str> {
    match provider {
        "codex" => Ok("gpt-6.2-sol"),
        "claude" => Ok("opus"),
        "antigravity" => Ok(AGY_FIXED_EFFORT_MODELS[0]),
        _ => Err(Error::new("unsupported provider orchestrator")),
    }
}

fn orchestrator_command(
    provider: &str,
    model: &str,
    effort: Option<&str>,
    permissions: MainPermissions,
) -> Result<Vec<OsString>> {
    let (program, permission_flag) = match provider {
        "codex" => ("codex", "--dangerously-bypass-approvals-and-sandbox"),
        "claude" => ("claude", "--dangerously-skip-permissions"),
        "antigravity" => ("agy", "--dangerously-skip-permissions"),
        _ => return Err(Error::new("unsupported provider orchestrator")),
    };
    native_model::validate_effort(provider, model, effort.unwrap_or_default())
        .map_err(Error::new)?;
    let mut command = commands::strings(&[program, "--model", model]);
    if permissions == MainPermissions::Full {
        command.push(permission_flag.into());
    }
    if let Some(effort) = effort
        .map(native_model::normalized_effort)
        .filter(|e| !e.is_empty())
    {
        if provider == "codex" {
            command.extend(commands::strings(&[
                "-c",
                &format!("model_reasoning_effort=\"{effort}\""),
            ]));
        } else {
            command.extend(commands::strings(&["--effort", effort]));
        }
    }
    if provider == "claude" {
        command.extend(commands::strings(&[
            "--append-system-prompt",
            STARTUP_PROMPT,
        ]));
    }
    Ok(command)
}

fn lead_permissions(main: &Value) -> Result<MainPermissions> {
    let inherited = MainPermissions::from_env()?;
    let recorded = main["refs"]
        .get(MAIN_PERMISSIONS_REF)
        .map(|mode| MainPermissions::parse(mode.as_str()))
        .transpose()?;
    // A launch environment can tighten the owning Main's recorded mode.
    if recorded == Some(MainPermissions::Native) {
        Ok(MainPermissions::Native)
    } else {
        Ok(inherited)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pane {
    window: String,
    id: String,
    pid: u32,
    active: bool,
}

const NATIVE_DOCK_ROWS: &str = "@oms_panel_native_dock_rows";
const NATIVE_DOCK_GEOMETRY: &str = concat!(
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t",
    "#{window_zoomed_flag}\t#{window_panes}\t#{window_width}\t#{window_height}\t",
    "#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{@oms_panel_main_attempt}\t.",
);

#[derive(Clone, Copy, PartialEq, Eq)]
struct NativeDockGeometry {
    window_size: (usize, usize),
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

/// Geometry observed in one census of the managed Main window.
#[derive(PartialEq, Eq)]
struct NativeDockWindow {
    board: NativeDockGeometry,
    main: NativeDockGeometry,
    dock: Option<NativeDockGeometry>,
}

impl NativeDockWindow {
    fn collapse(&self) -> Option<NativeDockWindow> {
        let dock = self.dock?;
        (dock.left == self.board.left
            && dock.width == self.board.width
            && dock.top == self.board.top + self.board.height + 1)
            .then(|| Self {
                board: NativeDockGeometry {
                    height: self.board.height + dock.height + 1,
                    ..self.board
                },
                main: self.main,
                dock: None,
            })
    }
}

pub struct Panel<'a> {
    repo: &'a Path,
    services: &'a dyn Services,
    stop: &'a AtomicBool,
    session: String,
    session_proof: terminals::SessionProof,
    caller: String,
    main: Pane,
    main_account_ready: Option<bool>,
    attempt: String,
    participant: String,
    owner: String,
    room: &'a str,
    leads: BTreeMap<String, Pane>,
    retained_codex: Option<Result<RetainedCodex>>,
    inherited_codex_parent: Option<String>,
    codex_chat_mark: CodexChatMark,
    claude_quota_mark: Option<String>,
    live_providers: BTreeSet<&'static str>,
    live_codex_panes: usize,
    alternate: Result<Option<Pane>>,
    #[cfg(test)]
    chat_host: Option<&'a dyn AccountHost>,
}

struct RetainedCodex {
    pane: Pane,
    parent: String,
    chat_mark: CodexChatMark,
}

// The last field keeps trailing columns intact and carries private launch markers.
pub(crate) const PANE_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_attempt}\t#{@oms_panel_owner}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_provider_session}\t#{@oms_provider_parent}\t#{pane_active}\t.#{@oms_panel_main_account_ready}_#{@oms_codex_chat_bound}_#{@oms_claude_quota_bound}";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CodexChatMark {
    #[default]
    Absent,
    Bound,
    Invalid,
}

impl CodexChatMark {
    fn from_census(field: &str) -> Self {
        match field.split_once('_').map(|(_, mark)| mark) {
            None | Some("" | "_") => Self::Absent,
            Some("1" | "1_") => Self::Bound,
            _ => Self::Invalid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexChatSource {
    Ambient,
    LegacyBound,
    Bound,
    Unavailable,
}

enum CodexChatRoot {
    Ambient(PathBuf, OwnedFd),
    Bound(CodexChatHome),
}

impl CodexChatRoot {
    fn parts(&self) -> (&Path, &OwnedFd) {
        match self {
            Self::Ambient(home, fd) => (home, fd),
            Self::Bound(proof) => (proof.home(), proof.directory()),
        }
    }

    fn same_root(&self, current: &Self) -> bool {
        match (self, current) {
            (Self::Bound(old), Self::Bound(new)) => {
                old.home() == new.home()
                    && new.matches_opened(old.directory())
                    && crate::native_children::codex_root_identity(new.home(), new.directory())
                        .is_some()
            }
            (Self::Ambient(old, fd), Self::Ambient(new, current)) if old == new => {
                let expected = crate::native_children::codex_root_identity(old, fd);
                expected.is_some()
                    && crate::native_children::codex_root_identity(new, current) == expected
            }
            _ => false,
        }
    }
}

struct CodexAssignment {
    repo: PathBuf,
    main: PaneIdentity,
    lead: PaneIdentity,
    binding: Option<PersistedAccountBinding>,
    root: Option<CodexChatRoot>,
}

impl CodexAssignment {
    fn same_custody(&self, other: &Self) -> bool {
        self.repo == other.repo
            && self.main == other.main
            && self.lead == other.lead
            && self.binding == other.binding
            && match (&self.root, &other.root) {
                (Some(old), Some(new)) => old.same_root(new),
                (None, None) => true,
                _ => false,
            }
    }
}

// Codex leads strip the inherited Room environment, so a locator must pin the state
// repository explicitly even when the lead changes its cwd or enters a worktree.
fn assignment_locator(repo: &Path, room: &str) -> Result<String> {
    let repo = repo
        .to_str()
        .ok_or_else(|| Error::new("repository cannot be represented in a Room locator"))?;
    Ok(format!(
        " Read-only Room locator: `oms room show --repo {} --id {} --exact --json`.",
        assignment_shell_word(repo),
        assignment_shell_word(room),
    ))
}

fn assignment_shell_word(value: &str) -> String {
    if !value.contains(|ch: char| ch.is_control() || ch == '`') {
        return commands::shell_quote(value);
    }
    // Preserve unusual path bytes without putting controls or code delimiters in the prompt.
    use std::fmt::Write;
    let mut word = String::from("$'");
    for byte in value.bytes() {
        write!(word, "\\{byte:03o}").expect("writing to a String cannot fail");
    }
    word.push('\'');
    word
}

fn legacy_assignment_root(
    host: &dyn AccountHost,
    binding: &PersistedAccountBinding,
) -> Option<CodexChatRoot> {
    let accounts = host.load_accounts().ok()??;
    let configured = accounts
        .home("codex", binding.account_id.as_deref()?)
        .ok()?;
    if std::fs::symlink_metadata(configured)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let (home, fd) = crate::native_children::open_ambient_codex_root(configured)?;
    let stat = rustix::fs::fstat(&fd).ok()?;
    if stat.st_uid != host.uid()
        || stat.st_mode & 0o022 != 0
        || binding.home_digest.as_deref() != Some(account_binding::path_digest(&home).as_str())
        || accounts.home_claimed_by_other("codex", &home)
    {
        return None;
    }
    crate::native_children::codex_root_identity(&home, &fd)?;
    Some(CodexChatRoot::Ambient(home, fd))
}

#[derive(Clone, Default)]
pub struct Observations {
    panes: BTreeMap<String, Observation>,
    chats: BTreeMap<String, ChatBinding>,
    codex_bound_generation: Option<ChatPane>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChatBinding {
    pane: ChatPane,
    locator: NativeLocator,
}

impl ChatBinding {
    #[cfg(test)]
    pub(crate) fn repo(&self) -> &Path {
        &self.pane.repo
    }
}

/// What a painted native child row reads its chat from, per provider.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum NativeLocator {
    Codex(crate::native_children::CodexLocator),
    Claude(crate::claude_child_chat::ClaudeChildLocator),
    Antigravity(crate::agy_child_chat::AgySelectedChild),
}

#[derive(Clone, PartialEq, Eq)]
struct ChatPane {
    provider: String,
    repo: PathBuf,
    room: String,
    attempt: String,
    owner: String,
    session: terminals::SessionProof,
    board: String,
    main: PaneIdentity,
    lead: PaneIdentity,
    bound_account: Option<(String, String)>,
    codex_source: Option<CodexChatSource>,
}

impl ChatPane {
    fn same_codex_generation(&self, current: &Self) -> bool {
        let same_pane = |a: &PaneIdentity, b: &PaneIdentity| {
            a.id == b.id && a.pid == b.pid && a.birth == b.birth
        };
        self.repo == current.repo
            && self.room == current.room
            && self.attempt == current.attempt
            && self.owner == current.owner
            && self.session == current.session
            && same_pane(&self.main, &current.main)
            && same_pane(&self.lead, &current.lead)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PaneIdentity {
    window: String,
    id: String,
    pid: u32,
    birth: String,
}

impl PaneIdentity {
    fn sample_with_host(pane: &Pane, host: &dyn AccountHost, owned: bool) -> Result<Self> {
        // Pane focus can change between painting the row and opening its transcript.
        let owner = host.process_owner(pane.pid)?;
        require(
            !owned || (owner.uid == host.uid() && owner.pid == pane.pid && !owner.birth.is_empty()),
            "provider process ownership could not be verified",
        )?;
        Ok(Self {
            window: pane.window.clone(),
            id: pane.id.clone(),
            pid: pane.pid,
            birth: owner.birth,
        })
    }
}

struct AlternateCaller {
    repo: PathBuf,
    room: String,
    attempt: String,
    participant: String,
    owner: String,
    campaign: bool,
}

impl AlternateCaller {
    fn from_env() -> Self {
        Self {
            repo: std::env::var_os("OMS_ROOM_REPO").unwrap_or_default().into(),
            room: std::env::var("OMS_ROOM_ID").unwrap_or_default(),
            attempt: std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap_or_default(),
            participant: std::env::var("OMS_ROOM_PARTICIPANT").unwrap_or_default(),
            owner: std::env::var("OMS_PANEL_OWNER").unwrap_or_default(),
            campaign: std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_some(),
        }
    }
}

struct BoundCodexPane {
    pane: PaneIdentity,
    receipt: PersistedAccountBinding,
    home: CodexChatHome,
}

impl BoundCodexPane {
    fn same_custody(&self, current: &Self) -> bool {
        self.pane == current.pane
            && self.receipt == current.receipt
            && self.home.same_directory(&current.home)
    }

    fn require_distinct(&self, account: Option<&str>, home: &CodexChatHome) -> Result<()> {
        require(
            self.receipt.account_id.as_deref() != account
                && self.home.home() != home.home()
                && !self.home.same_directory(home),
            "the Codex alternate lead requires a different account and home from the primary lead",
        )
    }
}

impl Clone for Observation {
    fn clone(&self) -> Self {
        use crate::native_children::AgyParent;
        Self {
            identity: self.identity.clone(),
            model: self.model.clone(),
            weekly: self.weekly,
            quota_visible: self.quota_visible,
            pending: self.pending,
            dismiss_pending_usage: self.dismiss_pending_usage,
            agy_parent: match &self.agy_parent {
                AgyParent::Unseen => AgyParent::Unseen,
                AgyParent::Legacy(path) => AgyParent::Legacy(path.clone()),
                AgyParent::Bounded => AgyParent::Bounded,
            },
        }
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct ObservationGeneration {
    session: terminals::SessionProof,
    main: PaneIdentity,
    leads: BTreeMap<String, PaneIdentity>,
    codex_chat_mark: CodexChatMark,
    claude_quota_mark: Option<String>,
    main_account_ready: Option<bool>,
    main_account: Option<ObservationAccount>,
    lead_accounts: [Option<ObservationAccount>; 3],
}

#[derive(PartialEq, Eq)]
struct ObservationAccount {
    binding: PersistedAccountBinding,
    directory: (rustix::fs::Dev, u64),
}

impl ObservationAccount {
    fn sample(host: &dyn AccountHost, binding: PersistedAccountBinding) -> Option<Self> {
        let accounts = host.load_accounts().ok()??;
        let alias = binding.account_id.as_deref()?;
        let provider = binding.provider.as_str();
        let configured = accounts.home(provider, alias).ok()?;
        let (home, root) = crate::native_children::open_ambient_codex_root(configured)?;
        let stat = rustix::fs::fstat(&root).ok()?;
        if stat.st_uid != host.uid()
            || stat.st_mode & 0o022 != 0
            || binding.home_digest.as_deref() != Some(account_binding::path_digest(&home).as_str())
            || accounts.home_claimed_by_other(provider, &home)
            || (provider == "claude"
                && !account_binding::claude_binding_still_mapped(host, &binding))
            || (binding.has_codex_chat_home_identity()
                && account_binding::bound_codex_home(host, &binding).is_none())
        {
            return None;
        }
        let directory = crate::native_children::codex_root_identity(&home, &root)?;
        Some(Self { binding, directory })
    }
}

impl ObservationGeneration {
    /// Whether `pane`, sampled from a fresh pane census, still shows this generation.
    fn fences(&self, pane: &ChatPane, provider: &str) -> bool {
        self.session == pane.session
            && self.main == pane.main
            && self.leads.get(provider) == Some(&pane.lead)
    }

    pub(crate) fn processes_current(&self) -> bool {
        let host = SystemAccountHost;
        host.process_owner(self.session.server_pid)
            .is_ok_and(|owner| {
                owner.uid == host.uid()
                    && owner.pid == self.session.server_pid
                    && owner.birth == self.session.server_birth
            })
            && std::iter::once(&self.main)
                .chain(self.leads.values())
                .all(|pane| {
                    host.process_owner(pane.pid).is_ok_and(|owner| {
                        owner.uid == host.uid()
                            && owner.pid == pane.pid
                            && owner.birth == pane.birth
                    })
                })
    }
}

impl Observations {
    pub(crate) fn chat_binding(&self, participant: &str) -> Option<&ChatBinding> {
        self.chats.get(participant)
    }

    pub(crate) fn clear_chats(&mut self) {
        self.chats.clear();
    }

    pub(crate) fn invalidate_readings(&mut self) {
        self.panes.clear();
        self.clear_chats();
    }

    fn accept_codex_source(&mut self, pane: &ChatPane, mark: CodexChatMark) -> bool {
        let legacy = pane.codex_source == Some(CodexChatSource::LegacyBound);
        let known_legacy = self
            .codex_bound_generation
            .as_ref()
            .filter(|bound| bound.same_codex_generation(pane))
            .map(|bound| bound.codex_source == Some(CodexChatSource::LegacyBound));
        // Remember evidence even when there is no Turn or the bound root cannot be opened.
        if mark == CodexChatMark::Bound || (pane.bound_account.is_some() && !legacy) {
            if known_legacy != Some(false) {
                self.codex_bound_generation = Some(pane.clone());
            }
            return pane.codex_source == Some(CodexChatSource::Bound);
        }
        if legacy && known_legacy.is_none() {
            self.codex_bound_generation = Some(pane.clone());
        }
        known_legacy.is_none() || (known_legacy == Some(true) && legacy)
    }

    pub(crate) fn take_usage_dismissals(&mut self) -> Self {
        Self {
            panes: self
                .panes
                .iter_mut()
                .filter_map(|(provider, sample)| {
                    let requested = sample.dismiss_pending_usage.take()?;
                    let mut action = sample.clone();
                    action.dismiss_pending_usage = Some(requested);
                    Some((provider.clone(), action))
                })
                .collect(),
            ..Self::default()
        }
    }

    pub(crate) fn has_usage_dismissal(&self) -> bool {
        self.panes
            .values()
            .any(|sample| sample.dismiss_pending_usage.is_some())
    }

    pub fn quota_pending(&self) -> bool {
        self.panes.values().any(|sample| {
            sample
                .pending
                .is_some_and(|started| started.elapsed() < Duration::from_secs(15))
        })
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct ObservationIdentity {
    pane_id: String,
    pid: u32,
    birth: String,
    canon_repo: PathBuf,
    room: String,
    main_attempt: String,
    account: Option<String>,
}

#[derive(Default)]
struct Observation {
    identity: ObservationIdentity,
    model: Option<String>,
    weekly: Option<(f64, f64)>,
    quota_visible: bool,
    pending: Option<Instant>,
    dismiss_pending_usage: Option<Instant>,
    agy_parent: crate::native_children::AgyParent,
}

impl<'a> Panel<'a> {
    pub fn load(
        repo: &'a Path,
        room: &'a str,
        services: &'a dyn Services,
        stop: &'a AtomicBool,
    ) -> Result<Self> {
        let caller = std::env::var("TMUX_PANE").unwrap_or_default();
        Self::load_for_caller(repo, room, &caller, services, stop)
    }

    fn load_for_caller(
        repo: &'a Path,
        room: &'a str,
        caller: &str,
        services: &'a dyn Services,
        stop: &'a AtomicBool,
    ) -> Result<Self> {
        Self::load_for_caller_with_pane_census(repo, room, caller, services, stop, None)
    }

    fn load_for_caller_with_pane_census(
        repo: &'a Path,
        room: &'a str,
        caller: &str,
        services: &'a dyn Services,
        stop: &'a AtomicBool,
        pane_windows: Option<&mut Vec<(String, String)>>,
    ) -> Result<Self> {
        require(
            commands::valid_target(caller, '%'),
            "open an OMS tmux panel first",
        )?;
        let raw = terminals::tmux(
            services,
            repo,
            &commands::strings(&["list-panes", "-s", "-t", caller, "-F", PANE_FORMAT]),
            stop,
        )?;
        let rows: Vec<Vec<&str>> = raw.lines().map(|line| line.split('\t').collect()).collect();
        let mut mains = rows.iter().filter(|f| {
            f.len() == 15
                && f[4] == "0"
                && f[6] == f[2]
                && f[10] == room
                && valid_attempt(f[7])
                && valid_attempt(f[9])
                && PROVIDERS.contains(&f[8])
        });
        let main = mains
            .next()
            .ok_or_else(|| Error::new("live room Main unavailable"))?;
        require(mains.next().is_none(), "multiple room Main panes found")?;
        let session = main[0].to_owned();
        let session_proof = terminals::provider_session_proof(services, repo, &session, stop)?;
        let mut leads = BTreeMap::new();
        let mut retained_codex = None;
        let mut codex_chat_mark = CodexChatMark::Absent;
        let mut claude_quota_mark = None;
        let mut live_providers = BTreeSet::new();
        let mut live_codex_panes = 0;
        let mut alternate = Ok(None);
        for f in &rows {
            let provider = live_provider(f, &session, main[2]);
            // Ignore retained dead panes. Verify exact window occupancy before using a candidate.
            if f.get(12) == Some(&ALTERNATE_LEAD_TAG) && f[4] != "1" {
                alternate = match alternate {
                    Ok(None)
                        if provider == Some("codex")
                            && f[1] != main[1]
                            && f[6..11].iter().all(|field| field.is_empty())
                            && matches!(f[14], "." | "._" | ".__") =>
                    {
                        pane(f).map(Some)
                    }
                    Err(_) => alternate,
                    _ => Err(Error::new(
                        "the Codex alternate lead has conflicting tmux scope",
                    )),
                };
            }
            let Some(provider) = provider else {
                continue;
            };
            live_providers.insert(provider);
            live_codex_panes += usize::from(provider == "codex");
            if f[10] != room {
                continue;
            }
            if f[12] != main[7] {
                if provider == "codex" && valid_attempt(f[12]) {
                    retained_codex = Some(if retained_codex.is_none() {
                        pane(f).map(|pane| RetainedCodex {
                            pane,
                            parent: f[12].into(),
                            chat_mark: CodexChatMark::from_census(f[14]),
                        })
                    } else {
                        Err(Error::new(
                            "multiple retained Codex panes require reconciliation",
                        ))
                    });
                }
                continue;
            }
            require(
                leads.insert(provider.into(), pane(f)?).is_none(),
                "multiple windows for the provider session",
            )?;
            if provider == "codex" {
                codex_chat_mark = CodexChatMark::from_census(f[14]);
            } else if provider == "claude" {
                claude_quota_mark = f[14]
                    .splitn(3, '_')
                    .nth(2)
                    .filter(|mark| !mark.is_empty())
                    .map(str::to_owned);
            }
        }
        if let Some(pane_windows) = pane_windows {
            for row in &rows {
                match row.as_slice() {
                    [_, window, pane, ..]
                        if commands::valid_target(window, '@')
                            && commands::valid_target(pane, '%') =>
                    {
                        pane_windows.push(((*window).into(), (*pane).into()));
                    }
                    _ => {
                        return Err(Error::new(
                            "the pre-launch tmux pane census could not be verified",
                        ));
                    }
                }
            }
        }
        Ok(Self {
            repo,
            services,
            stop,
            session,
            session_proof,
            caller: caller.into(),
            main: pane(main)?,
            main_account_ready: match main[14] {
                "." | "._" | ".__" => None,
                ".1" | ".1_" | ".1__" => Some(true),
                _ => Some(false),
            },
            attempt: main[7].into(),
            participant: main[9].into(),
            owner: main[8].into(),
            room,
            leads,
            retained_codex,
            inherited_codex_parent: None,
            codex_chat_mark,
            claude_quota_mark,
            live_providers,
            live_codex_panes,
            alternate,
            #[cfg(test)]
            chat_host: None,
        })
    }

    /// Only an explicit native assignment command may inspect a retained
    /// candidate under its own Room, contract and live process checks.
    fn with_native_codex_candidate(mut self) -> Result<Self> {
        if !self.leads.contains_key("codex")
            && let Some(candidate) = self.retained_codex.take().transpose()?
        {
            self.leads.insert("codex".into(), candidate.pane);
            self.inherited_codex_parent = Some(candidate.parent);
            self.codex_chat_mark = candidate.chat_mark;
        }
        Ok(self)
    }

    pub fn connected(&self) -> Value {
        json!(self.leads.keys().collect::<Vec<_>>())
    }

    fn binding_with_birth(
        &self,
        role: &str,
        provider: &str,
        pane: &Pane,
        birth: &str,
        host: &dyn AccountHost,
    ) -> Option<PersistedAccountBinding> {
        account_binding::load_verified_binding(
            host,
            role,
            &self.binding_anchor(provider, pane, birth),
        )
    }

    fn binding_anchor<'b>(
        &'b self,
        provider: &'b str,
        pane: &'b Pane,
        birth: &'b str,
    ) -> BindingAnchor<'b> {
        BindingAnchor {
            repo: self.repo,
            room: self.room,
            main_attempt: &self.attempt,
            main_owner: &self.owner,
            session: &self.session_proof,
            provider,
            pane_id: &pane.id,
            pid: pane.pid,
            birth,
        }
    }

    fn live_binding(
        &self,
        provider: &str,
        pane: &Pane,
        host: &dyn AccountHost,
    ) -> Option<PersistedAccountBinding> {
        let owner = host.process_owner(pane.pid).ok()?;
        if owner.uid != host.uid() {
            return None;
        }
        self.binding_with_birth("orchestrator", provider, pane, &owner.birth, host)
            .or_else(|| {
                (provider == "codex" && self.attempt != self.participant)
                    .then(|| self.codex_assignment_custody(host).ok()?.binding)
                    .flatten()
            })
    }

    fn observation_state(
        &self,
        provider: &str,
        pane: &Pane,
        host: &dyn AccountHost,
    ) -> (ObservationIdentity, Option<PersistedAccountBinding>) {
        let owner = host
            .process_owner(pane.pid)
            .ok()
            .filter(|o| o.uid == host.uid());
        let birth = owner.as_ref().map(|o| o.birth.clone()).unwrap_or_default();
        let binding = owner
            .as_ref()
            .and_then(|o| self.binding_with_birth("orchestrator", provider, pane, &o.birth, host));
        let canon_repo =
            std::fs::canonicalize(self.repo).unwrap_or_else(|_| self.repo.to_path_buf());
        let account = binding
            .as_ref()
            .filter(|b| b.verification == AccountVerification::Verified)
            .and_then(|b| b.account_id.clone());
        let identity = ObservationIdentity {
            pane_id: pane.id.clone(),
            pid: pane.pid,
            birth,
            canon_repo,
            room: self.room.to_owned(),
            main_attempt: self.attempt.clone(),
            account,
        };
        (identity, binding)
    }

    fn codex_main_account(
        &self,
        report: &Value,
        host: &dyn AccountHost,
    ) -> Option<(String, String)> {
        let main = &report["room_attempts"][&self.attempt];
        let refs = &main["refs"];
        if self.main_account_ready != Some(true)
            || !self.has_live_main_membership(&report["room"])
            || !self.is_live_main(main)
            || refs["panel_owner"] != "codex"
            || refs["panel_harness"] != "codex"
            || refs
                .get("panel_room_participant")
                .is_some_and(|member| member != &self.participant)
        {
            return None;
        }
        let owner = host.process_owner(self.main.pid).ok()?;
        if owner.uid != host.uid() || owner.pid != self.main.pid {
            return None;
        }
        let binding = self.binding_with_birth("main", "codex", &self.main, &owner.birth, host)?;
        if binding.verification != AccountVerification::Verified || !binding.account_pinned {
            return None;
        }
        binding.account_id.zip(binding.home_digest)
    }

    pub(crate) fn observation_generation(&self) -> Result<ObservationGeneration> {
        self.observation_generation_with_host(&SystemAccountHost)
    }

    fn observation_generation_with_host(
        &self,
        host: &dyn AccountHost,
    ) -> Result<ObservationGeneration> {
        let main = PaneIdentity::sample_with_host(&self.main, host, true)?;
        let leads = self
            .leads
            .iter()
            .map(|(provider, pane)| {
                Ok((
                    provider.clone(),
                    PaneIdentity::sample_with_host(pane, host, true)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let account = |role, provider, pane, identity: &PaneIdentity| {
            self.binding_with_birth(role, provider, pane, &identity.birth, host)
                .filter(|binding| binding.verification == AccountVerification::Verified)
                .map(|binding| {
                    ObservationAccount::sample(host, binding)
                        .ok_or_else(|| Error::new("verified provider account mapping changed"))
                })
                .transpose()
        };
        let main_account = if matches!(self.owner.as_str(), "codex" | "claude")
            && self.main_account_ready == Some(true)
        {
            account("main", &self.owner, &self.main, &main)?
        } else {
            None
        };
        let lead_account = |provider| match self.leads.get(provider).zip(leads.get(provider)) {
            Some((pane, identity)) => account("orchestrator", provider, pane, identity),
            None => Ok(None),
        };
        let lead_accounts = [
            lead_account("codex")?,
            lead_account("claude")?,
            lead_account("antigravity")?,
        ];
        Ok(ObservationGeneration {
            session: self.session_proof.clone(),
            main,
            leads,
            main_account,
            lead_accounts,
            codex_chat_mark: self.codex_chat_mark,
            claude_quota_mark: self.claude_quota_mark.clone(),
            main_account_ready: self.main_account_ready,
        })
    }

    /// The Board's read: `expected` is the generation its render fence will
    /// require, so a provider pane that already left it gets no usage reads.
    pub(crate) fn observe_passively(
        &self,
        readings: &mut Observations,
        report: &mut Value,
        expected: &ObservationGeneration,
    ) {
        self.observe_reading(
            readings,
            report,
            true,
            &SystemAccountHost,
            crate::claude_lead_usage::bound_claude_lead_weekly,
            Some(expected),
        );
    }

    pub fn observe(&self, readings: &mut Observations, report: &mut Value, chat_current: bool) {
        self.observe_with_host(readings, report, chat_current, &SystemAccountHost);
    }

    pub(crate) fn observe_with_host(
        &self,
        readings: &mut Observations,
        report: &mut Value,
        chat_current: bool,
        host: &dyn AccountHost,
    ) {
        self.observe_with_claude_reader(
            readings,
            report,
            chat_current,
            host,
            crate::claude_lead_usage::bound_claude_lead_weekly,
        );
    }

    fn observe_with_claude_reader(
        &self,
        readings: &mut Observations,
        report: &mut Value,
        chat_current: bool,
        host: &dyn AccountHost,
        claude_reader: impl Fn(&Owner, &str, f64) -> Option<LeadReading>,
    ) {
        self.observe_reading(readings, report, chat_current, host, claude_reader, None);
    }

    fn observe_reading(
        &self,
        readings: &mut Observations,
        report: &mut Value,
        chat_current: bool,
        host: &dyn AccountHost,
        claude_reader: impl Fn(&Owner, &str, f64) -> Option<LeadReading>,
        // Present only for the Board's passive read, which never sends keys.
        expected: Option<&ObservationGeneration>,
    ) {
        let quota_sources =
            commands::take_codex_quota_receipt(self.repo, self.room, &self.attempt, report);
        let now = oms_panel_core::timestamp() as f64;
        report["provider_sessions"] = self.connected();
        report["provider_models"] = json!({});
        report["provider_activity"] = json!({});
        report["provider_accounts"] = json!({});
        if let Some(fields) = report.as_object_mut() {
            fields.remove("claude_local_weekly");
            fields.remove("codex_weekly_account");
            fields.remove("agy_footer_weekly");
        }
        report["provider_delivery"] = json!({});
        let delivery_store = self
            .has_live_report_scope(report)
            .then(|| Store::open(self.repo).ok())
            .flatten();
        let mut children = Vec::new();
        let census = std::cell::OnceCell::new();
        let mut bound_codex = None;
        readings.clear_chats();
        let mut codex_chat_pane = self
            .chat_pane_with_host("codex", host)
            .ok()
            .filter(|pane| readings.accept_codex_source(pane, self.codex_chat_mark));
        readings
            .panes
            .retain(|provider, _| self.leads.contains_key(provider));
        for (provider, pane) in &self.leads {
            if let Some(delivery) = delivery_store.as_ref().and_then(|store| {
                lead_delivery::Receipt::for_live_lead(store, self, provider, host)
            }) {
                report["provider_delivery"][provider] = json!(delivery);
            }
            let (identity, binding) = self.observation_state(provider, pane, host);
            let sample = readings
                .panes
                .entry(provider.clone())
                .or_insert_with(|| Observation {
                    identity: identity.clone(),
                    ..Observation::default()
                });
            if sample.identity != identity {
                *sample = Observation {
                    identity,
                    ..Observation::default()
                };
            }
            let (account_id, account_pinned, verification, bound_digest) = match &binding {
                Some(b) => (
                    b.account_id.clone(),
                    b.account_pinned,
                    b.verification,
                    b.home_digest.clone(),
                ),
                None => (None, false, AccountVerification::Unverified, None),
            };
            report["provider_accounts"][provider] = json!({
                "account": account_id,
                "pinned": account_pinned,
                "verification": verification,
            });
            if let Ok(screen) = self.tmux(&["capture-pane", "-p", "-t", &pane.id]) {
                let activity = crate::provider_activity::native_activity(provider, &screen);
                let detail = activity.and_then(crate::provider_activity::NativeActivity::detail);
                let same_pane_process =
                    (provider == "antigravity" || provider == "claude" || detail.is_some())
                        && !sample.identity.birth.is_empty()
                        && host.process_owner(pane.pid).is_ok_and(|identity| {
                            identity.pid == pane.pid
                                && identity.uid == host.uid()
                                && identity.birth == sample.identity.birth
                        });
                if activity.is_some() {
                    report["provider_activity"][provider] =
                        detail.filter(|_| same_pane_process).map_or_else(
                            || json!("working"),
                            |detail| json!({"state":"working","detail":detail}),
                        );
                }
                if provider == "antigravity" && same_pane_process {
                    let children = crate::provider_activity::agy_roster(&screen);
                    if !children.is_empty() {
                        let activity = &mut report["provider_activity"][provider];
                        if !activity.is_object() {
                            *activity = json!({"state":"working"});
                        }
                        activity["children"] = json!(children);
                    }
                    let report_current = !self.stop.load(Ordering::Acquire)
                        && report["capacity_stale"] != true
                        && report
                            .get("collection")
                            .is_none_or(|c| c["ok"] == true && c["pending"] != true);
                    let same_account = report_current
                        && self.binding_with_birth(
                            "orchestrator",
                            "antigravity",
                            pane,
                            &sample.identity.birth,
                            host,
                        ) == binding;
                    if same_account
                        && let Some(remaining) =
                            crate::provider_activity::agy_footer_weekly(&screen)
                    {
                        report["agy_footer_weekly"] = json!({"remaining": remaining});
                    }
                }
                let model = (provider == "codex")
                    .then(|| codex_model_from_chrome(&screen))
                    .flatten()
                    .or_else(|| observed_model(provider, &screen).map(str::to_owned));
                if let Some(model) = model {
                    sample.model = Some(model);
                }
                let current = weekly_from_usage(provider, &screen);
                if let Some(used) = current {
                    if !sample.quota_visible || sample.weekly.is_none_or(|(last, _)| last != used) {
                        sample.weekly = Some((used, now));
                    }
                    let pending = sample
                        .pending
                        .take()
                        .filter(|started| started.elapsed() < Duration::from_secs(15));
                    if let Some(started) = pending {
                        if expected.is_some() {
                            sample.dismiss_pending_usage = Some(started);
                        } else if !pane.active
                            && self.reload().is_ok_and(|fresh| {
                                fresh.leads.get(provider) == Some(pane)
                                    && fresh.require_board(None).is_ok()
                            })
                        {
                            let _ = self.tmux(&["send-keys", "-t", &pane.id, "Escape"]);
                        }
                    }
                }
                sample.quota_visible = current.is_some();
                if provider == "codex" {
                    if let Some((chat_pane, observed)) = codex_chat_pane
                        .take()
                        .and_then(|pane| self.observe_codex_children(pane, report, host))
                    {
                        if chat_current {
                            readings.chats.extend(observed.codex.into_iter().map(
                                |(participant, locator)| {
                                    (
                                        participant,
                                        ChatBinding {
                                            pane: chat_pane.clone(),
                                            locator: NativeLocator::Codex(locator),
                                        },
                                    )
                                },
                            ));
                        }
                        children.extend(observed.rows);
                    }
                } else if PROVIDERS.contains(&provider.as_str()) {
                    let chat_pane = (provider == "claude" && chat_current)
                        .then(|| self.chat_pane_with_host(provider, host).ok())
                        .flatten()
                        .filter(|_| self.still_observes(expected, &census, provider, host));
                    let recent = match provider.as_str() {
                        "antigravity" => self.capture_recent(&pane.id).unwrap_or(screen),
                        _ => screen,
                    };
                    let mut observed = crate::native_children::observe(
                        provider,
                        pane.pid,
                        self.repo,
                        &recent,
                        &mut sample.agy_parent,
                    );
                    if provider == "claude" {
                        let mut located = Vec::new();
                        let sighting = (provider == "claude" && chat_pane.is_some())
                            .then(|| {
                                crate::claude_child_chat::claude_board_children(pane.pid, self.repo)
                            })
                            .flatten();
                        let second = chat_pane
                            .as_ref()
                            .filter(|_| {
                                !located.is_empty()
                                    || sighting.as_ref().is_some_and(|(_, c)| !c.is_empty())
                            })
                            .and_then(|_| self.chat_pane_with_host(provider, host).ok());
                        if let Some(chat_pane) = chat_pane {
                            let (rows, claude) = claude_sighting(
                                std::mem::take(&mut observed.rows),
                                &chat_pane,
                                second.as_ref(),
                                sighting,
                            );
                            observed.rows = rows;
                            located.extend(claude);
                            if !located.is_empty() && second.as_ref() == Some(&chat_pane) {
                                readings.chats.extend(located.into_iter().map(
                                    |(participant, locator)| {
                                        (
                                            participant,
                                            ChatBinding {
                                                pane: chat_pane.clone(),
                                                locator,
                                            },
                                        )
                                    },
                                ));
                            }
                        }
                        let roster_current = same_pane_process
                            && !self.stop.load(Ordering::Acquire)
                            && report["capacity_stale"] != true
                            && report
                                .get("collection")
                                .is_none_or(|c| c["ok"] == true && c["pending"] != true)
                            && self.still_observes(expected, &census, provider, host);
                        project_claude_pane_roster(
                            roster_current,
                            &recent,
                            activity.is_some(),
                            &mut observed.rows,
                            &mut report["provider_activity"],
                        );
                    } else if provider == "antigravity"
                        && chat_current
                        && !observed.rows.is_empty()
                        && self.has_live_report_scope(report)
                        && !self.stop.load(Ordering::Acquire)
                        && let Some(shown_turn) = crate::native_children::agy_current_turn(&recent)
                        && let Ok(pane) = self.chat_pane_with_host("antigravity", host)
                        && self.still_observes(expected, &census, provider, host)
                        && !self.stop.load(Ordering::Acquire)
                    {
                        let bound = agy_sighting(
                            &observed.rows,
                            shown_turn,
                            pane.lead.pid,
                            &pane.lead.birth,
                        );
                        agy_usage_sighting(
                            &mut observed.rows,
                            shown_turn,
                            |painted| {
                                crate::agy_child_chat::agy_current_children_with_usage(
                                    pane.lead.pid,
                                    &pane.lead.birth,
                                    &pane.repo,
                                    Some(shown_turn),
                                    painted,
                                    true,
                                )
                                .map(|(turn, _, usage)| (turn, usage))
                            },
                            || {
                                !self.stop.load(Ordering::Acquire)
                                    && self
                                        .chat_pane_with_host("antigravity", host)
                                        .is_ok_and(|again| again == pane)
                            },
                        );
                        readings
                            .chats
                            .extend(bound.into_iter().map(|(participant, locator)| {
                                (
                                    participant,
                                    ChatBinding {
                                        pane: pane.clone(),
                                        locator,
                                    },
                                )
                            }));
                    }
                    children.extend(observed.rows);
                }
            }
            if verification == AccountVerification::Verified
                && let Some(acct) = account_id.as_deref()
            {
                let formatted = match &sample.model {
                    Some(model) => format!("{model} @{acct}"),
                    None => format!("@{acct}"),
                };
                report["provider_models"][provider] = json!(formatted);
            } else if let Some(model) = &sample.model {
                report["provider_models"][provider] = json!(model);
            }
            if provider == "claude"
                && (self.claude_quota_mark.is_some()
                    || verification == AccountVerification::Verified)
            {
                let precise = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(now, |elapsed| elapsed.as_secs_f64());
                let proved = binding.as_ref().and_then(|binding| {
                    let reading = self.pinned_claude_reading(
                        pane,
                        binding,
                        report,
                        host,
                        &claude_reader,
                        precise,
                    )?;
                    Some((binding.account_id.as_deref()?, reading))
                });
                project_pinned_claude_quota(&mut report["provider_status"][provider], proved, true);
                #[cfg(target_os = "linux")]
                if let Some(local) = binding
                    .as_ref()
                    .and_then(|binding| self.auto_claude_reading(pane, binding, report, host))
                {
                    report["claude_local_weekly"] = local;
                }
                continue;
            }
            if provider == "claude" && !sample.quota_visible {
                let precise = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(now, |elapsed| elapsed.as_secs_f64());
                if let Some(lead) = crate::claude_lead_usage::claude_lead_weekly(pane.pid, precise)
                {
                    merge_weekly(
                        &mut report["provider_status"][provider],
                        sample.weekly,
                        Some(&lead),
                    );
                    continue;
                }
            }
            if let Some((used, observed_at)) = sample.weekly {
                let status = &mut report["provider_status"][provider];
                if verification == AccountVerification::Verified
                    && let Some(alias) = account_id.as_deref()
                {
                    if let Some(acct) = status["accounts"]
                        .as_object_mut()
                        .and_then(|accounts| accounts.get_mut(alias))
                        .filter(|acct| acct.is_object())
                        && oms_panel_core::metrics::stamp(&acct["observed_at"])
                            .is_none_or(|old| old < observed_at)
                    {
                        acct["weekly_used"] = json!(used);
                        acct["weekly_native_observed_at"] = json!(observed_at);
                    }
                    let label_matches = status["account_label"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .is_none_or(|label| label == alias);
                    if label_matches
                        && oms_panel_core::metrics::stamp(&status["observed_at"])
                            .is_none_or(|old| old < observed_at)
                    {
                        status["weekly_used"] = json!(used);
                        status["weekly_native_observed_at"] = json!(observed_at);
                    }
                } else if oms_panel_core::metrics::stamp(&status["observed_at"])
                    .is_none_or(|old| old < observed_at)
                {
                    status["weekly_used"] = json!(used);
                    status["weekly_native_observed_at"] = json!(observed_at);
                }
            }
            if provider == "codex" && verification == AccountVerification::Verified {
                bound_codex = account_id.map(|alias| (alias, bound_digest));
            }
        }
        if self.owner == "claude"
            && self.main_account_ready.is_some()
            && !self.leads.contains_key("claude")
        {
            let (alias, reading) = self
                .pinned_claude_main_reading(report, host, &claude_reader)
                .unzip();
            project_pinned_claude_quota(
                &mut report["provider_status"]["claude"],
                alias.as_deref().zip(reading),
                false,
            );
        }
        if self.owner == "codex"
            && self.main_account_ready.is_some()
            && !self.leads.contains_key("codex")
        {
            // A marked Main with missing proof must suppress ambient quota too.
            bound_codex = Some(
                quota_sources
                    .as_ref()
                    .and_then(|_| self.codex_main_account(report, host))
                    .map(|(alias, digest)| (alias, Some(digest)))
                    .unwrap_or_default(),
            );
        }
        if let Some((alias, digest)) = bound_codex {
            let quota_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(now, |elapsed| elapsed.as_secs_f64());
            let live_scope = !self.stop.load(Ordering::Acquire)
                && report["capacity_stale"] != true
                && self.has_live_report_scope(report)
                && host.process_owner(self.main.pid).is_ok_and(|owner| {
                    owner.uid == host.uid() && owner.pid == self.main.pid && !owner.birth.is_empty()
                });
            let source = quota_sources
                .as_ref()
                .filter(|_| live_scope)
                .zip(digest.as_deref());
            project_bound_codex_quota(
                report,
                &alias,
                quota_now,
                source.is_some_and(|(sources, digest)| sources.account_matches(&alias, digest)),
                source.is_some_and(|(sources, digest)| sources.ambient_matches(digest)),
            );
        }
        report["native_children"] = json!(children);
    }

    fn pinned_claude_reading(
        &self,
        pane: &Pane,
        binding: &PersistedAccountBinding,
        report: &Value,
        host: &dyn AccountHost,
        reader: &impl Fn(&Owner, &str, f64) -> Option<LeadReading>,
        now: f64,
    ) -> Option<LeadReading> {
        let main = &report["room_attempts"][&self.attempt];
        if self.stop.load(Ordering::Acquire)
            || self
                .claude_quota_mark
                .as_deref()
                .is_some_and(|mark| mark != "1")
            || binding.verification != AccountVerification::Verified
            || !binding.account_pinned
            || report["collection"]["ok"] != true
            || report["collection"]["pending"] == true
            || !self.has_live_main_membership(&report["room"])
            || !self.is_live_main(main)
            || main["refs"]
                .get("panel_room_participant")
                .is_some_and(|member| member != &self.participant)
            || !account_binding::claude_binding_still_mapped(host, binding)
        {
            return None;
        }
        let owner = host.process_owner(pane.pid).ok()?;
        if owner.pid != pane.pid || owner.birth != binding.birth || owner.uid != host.uid() {
            return None;
        }
        let main_owner = host.process_owner(self.main.pid).ok()?;
        if main_owner.pid != self.main.pid
            || main_owner.uid != host.uid()
            || main_owner.birth.is_empty()
        {
            return None;
        }
        let reading = reader(&owner, binding.home_digest.as_deref()?, now);
        #[cfg(target_os = "linux")]
        let local = account_binding::claude_observation_home(host, binding)
            .and_then(|(home, identity)| {
                crate::claude_lead_usage::LocalObservation::read(&owner, &home, identity)
                    .map(|candidate| (home, candidate))
            })
            .filter(|_| report["capacity_stale"] != true);
        #[cfg(target_os = "linux")]
        if reading.is_none() && local.is_none() {
            return None;
        }
        #[cfg(not(target_os = "linux"))]
        reading.as_ref()?;
        let fresh = self.reload().ok()?;
        let same_pane = |a: &Pane, b: &Pane| a.window == b.window && a.id == b.id && a.pid == b.pid;
        let valid = fresh.session_proof == self.session_proof
            && same_pane(&fresh.main, &self.main)
            && fresh.attempt == self.attempt
            && fresh.owner == self.owner
            && fresh.claude_quota_mark == self.claude_quota_mark
            && fresh
                .leads
                .get("claude")
                .is_some_and(|lead| same_pane(lead, pane))
            && host.process_owner(self.main.pid).ok()? == main_owner
            && host.process_owner(pane.pid).ok()? == owner
            && fresh
                .binding_with_birth("orchestrator", "claude", pane, &owner.birth, host)
                .as_ref()
                == Some(binding)
            && account_binding::claude_binding_still_mapped(host, binding)
            && !self.stop.load(Ordering::Acquire);
        if !valid {
            return None;
        }
        #[cfg(target_os = "linux")]
        if let Some((home, candidate)) = local
            && let Some(alias) = binding.account_id.as_deref()
            && fresh.require_board(None).is_ok()
            && account_binding::claude_observation_home(host, binding)
                .is_some_and(|(current, _)| current == home)
            && !self.stop.load(Ordering::Acquire)
        {
            let _ = candidate.publish(&owner, &home, alias);
        }
        reading
    }

    #[cfg(target_os = "linux")]
    fn auto_claude_reading(
        &self,
        pane: &Pane,
        binding: &PersistedAccountBinding,
        report: &Value,
        host: &dyn AccountHost,
    ) -> Option<Value> {
        let launch = binding.claude_auto_observation.as_ref()?;
        let marker = launch.marker();
        let main = &report["room_attempts"][&self.attempt];
        if self.claude_quota_mark.as_deref() != Some(marker.as_str())
            || self.stop.load(Ordering::Acquire)
            || report["collection"]["ok"] != true
            || report["collection"]["pending"] == true
            || report["capacity_stale"] == true
            || !self.has_live_main_membership(&report["room"])
            || !self.is_live_main(main)
            || main["refs"]
                .get("panel_room_participant")
                .is_some_and(|member| member != &self.participant)
        {
            return None;
        }
        let owner = host.process_owner(pane.pid).ok()?;
        let main_owner = host.process_owner(self.main.pid).ok()?;
        if owner.pid != pane.pid
            || owner.uid != host.uid()
            || owner.birth != binding.birth
            || main_owner.pid != self.main.pid
            || main_owner.uid != host.uid()
            || main_owner.birth.is_empty()
        {
            return None;
        }
        let home = account_binding::claude_auto_observation_home(host, binding)?;
        let observation = crate::claude_lead_usage::AutoObservation::read(&owner, &home, launch)?;
        let fresh = self.reload().ok()?;
        let same_pane = |a: &Pane, b: &Pane| a.window == b.window && a.id == b.id && a.pid == b.pid;
        if fresh.session_proof != self.session_proof
            || fresh.attempt != self.attempt
            || fresh.owner != self.owner
            || !same_pane(&fresh.main, &self.main)
            || fresh.claude_quota_mark != self.claude_quota_mark
            || !fresh
                .leads
                .get("claude")
                .is_some_and(|lead| same_pane(lead, pane))
            || fresh.require_board(None).is_err()
            || host.process_owner(self.main.pid).ok()? != main_owner
            || fresh
                .binding_with_birth("orchestrator", "claude", pane, &owner.birth, host)
                .as_ref()
                != Some(binding)
            || account_binding::claude_auto_observation_home(host, binding)? != home
            || self.stop.load(Ordering::Acquire)
        {
            return None;
        }
        let reading = observation.finish(&owner, &home, launch)?;
        (!self.stop.load(Ordering::Acquire)).then_some(reading)
    }

    fn pinned_claude_main_reading(
        &self,
        report: &Value,
        host: &dyn AccountHost,
        reader: &impl Fn(&Owner, &str, f64) -> Option<LeadReading>,
    ) -> Option<(String, LeadReading)> {
        let main = &report["room_attempts"][&self.attempt];
        let refs = &main["refs"];
        if self.stop.load(Ordering::Acquire)
            || self.main_account_ready != Some(true)
            || report["collection"]["ok"] != true
            || report["collection"]["pending"] == true
            || report["capacity_stale"] == true
            || !self.has_live_main_membership(&report["room"])
            || !self.is_live_main(main)
            || refs["panel_owner"] != "claude"
            || refs["panel_harness"] != "claude"
            || refs
                .get("panel_room_participant")
                .is_some_and(|member| member != &self.participant)
        {
            return None;
        }
        let wrapper = host.process_owner(self.main.pid).ok()?;
        if wrapper.pid != self.main.pid || wrapper.uid != host.uid() || wrapper.birth.is_empty() {
            return None;
        }
        let binding =
            self.binding_with_birth("main", "claude", &self.main, &wrapper.birth, host)?;
        let child = binding.claude_main_child_owner(host)?;
        if !account_binding::claude_binding_still_mapped(host, &binding) {
            return None;
        }
        let current_time = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs_f64())
        };
        let now = current_time()?;
        let reading = reader(&child, binding.home_digest.as_deref()?, now)?;
        let fresh_at = |time: f64| {
            !reading.stale
                && reading
                    .weekly_used
                    .is_some_and(|used| (0.0..=100.0).contains(&used))
                && (0.0..=time).contains(&reading.observed_at)
                && time - reading.observed_at <= oms_panel_core::metrics::READING_FRESH_SECONDS
                && reading
                    .resets_at
                    .is_none_or(|reset| reset.is_finite() && reset > time)
        };
        if !fresh_at(now) || self.stop.load(Ordering::Acquire) {
            return None;
        }
        let room = commands::query(
            self.services,
            NativeCommand::Room,
            self.repo,
            &["show", "--id", self.room, "--exact", "--json"],
            self.stop,
        )
        .ok()?;
        let fresh = self.reload().ok()?;
        (fresh.has_live_main_membership(&room)
            && fresh.session_proof == self.session_proof
            && fresh.main.window == self.main.window
            && fresh.main.id == self.main.id
            && fresh.main.pid == self.main.pid
            && fresh.attempt == self.attempt
            && fresh.owner == self.owner
            && fresh.main_account_ready == Some(true)
            && !fresh.leads.contains_key("claude")
            && fresh
                .binding_with_birth("main", "claude", &fresh.main, &wrapper.birth, host)
                .as_ref()
                == Some(&binding)
            && account_binding::claude_binding_still_mapped(host, &binding)
            && host.process_owner(self.main.pid).ok()? == wrapper
            && binding.claude_main_child_owner(host).as_ref() == Some(&child)
            && fresh_at(current_time()?)
            && !self.stop.load(Ordering::Acquire))
        .then_some((binding.account_id?, reading))
    }

    pub fn refresh_usage(&self, readings: &mut Observations) -> usize {
        if self.require_board(None).is_err() {
            return 0;
        }
        let mut started = 0;
        for provider in ["claude", "antigravity"] {
            let Some(pane) = self.leads.get(provider) else {
                continue;
            };
            if self
                .send(provider, "/usage", Proof::Dispatched, None)
                .is_ok()
            {
                let (identity, _) = self.observation_state(provider, pane, &SystemAccountHost);
                let sample = readings
                    .panes
                    .entry(provider.into())
                    .or_insert_with(|| Observation {
                        identity: identity.clone(),
                        ..Observation::default()
                    });
                if sample.identity != identity {
                    *sample = Observation {
                        identity,
                        ..Observation::default()
                    };
                }
                sample.pending = Some(Instant::now());
                sample.dismiss_pending_usage = None;
                sample.quota_visible = false;
                started += 1;
            }
        }
        started
    }

    pub(crate) fn dismiss_observed_usage(&self, readings: &mut Observations) {
        for (provider, sample) in &mut readings.panes {
            if sample
                .dismiss_pending_usage
                .take()
                .is_none_or(|started| started.elapsed() >= Duration::from_secs(15))
                || !sample.quota_visible
            {
                continue;
            }
            let Some(pane) = self.leads.get(provider).filter(|pane| !pane.active) else {
                continue;
            };
            if self.require_board(None).is_ok()
                && self
                    .tmux(&["capture-pane", "-p", "-t", &pane.id])
                    .is_ok_and(|screen| weekly_from_usage(provider, &screen).is_some())
                && self.reload().is_ok_and(|fresh| {
                    fresh.leads.get(provider) == Some(pane)
                        && fresh.require_board(None).is_ok()
                        && fresh
                            .observation_state(provider, pane, &SystemAccountHost)
                            .0
                            == sample.identity
                })
            {
                let _ = self.tmux(&["send-keys", "-t", &pane.id, "Escape"]);
            }
        }
    }

    fn tmux(&self, args: &[&str]) -> Result<String> {
        terminals::tmux(
            self.services,
            self.repo,
            &commands::strings(args),
            self.stop,
        )
    }

    fn reload(&self) -> Result<Self> {
        self.reload_with_pane_census(None)
    }

    fn reload_with_pane_census(
        &self,
        pane_windows: Option<&mut Vec<(String, String)>>,
    ) -> Result<Self> {
        let panel = Self::load_for_caller_with_pane_census(
            self.repo,
            self.room,
            &self.caller,
            self.services,
            self.stop,
            pane_windows,
        )?;
        #[cfg(test)]
        let panel = Self {
            chat_host: self.chat_host,
            ..panel
        };
        if self.inherited_codex_parent.is_some() {
            panel.with_native_codex_candidate()
        } else {
            Ok(panel)
        }
    }

    fn capture_recent(&self, pane: &str) -> Result<String> {
        self.tmux(&["capture-pane", "-p", "-J", "-S", "-100", "-t", pane])
    }

    fn chat_pane(&self, provider: &str) -> Result<ChatPane> {
        self.chat_pane_with_host(provider, self.chat_account_host())
    }

    fn chat_account_host(&self) -> &dyn AccountHost {
        #[cfg(test)]
        if let Some(host) = self.chat_host {
            return host;
        }
        &SystemAccountHost
    }

    /// A frame whose provider pane moved during this read (say while its
    /// capture stalled) fails the render fence, so its transcript usage and
    /// chat bindings are skipped. The fresh census (two tmux calls) runs at most
    /// once per read, right before the first fenced usage read, and serves both
    /// providers; a pane that moves after it changes during the read, which the
    /// Board's final render fence still rejects.
    fn still_observes(
        &self,
        expected: Option<&ObservationGeneration>,
        census: &std::cell::OnceCell<Option<Self>>,
        provider: &str,
        host: &dyn AccountHost,
    ) -> bool {
        expected.is_none_or(|generation| {
            census
                .get_or_init(|| self.reload().ok())
                .as_ref()
                .and_then(|fresh| fresh.chat_pane_with_host(provider, host).ok())
                .is_some_and(|pane| generation.fences(&pane, provider))
        })
    }

    fn chat_pane_with_host(&self, provider: &str, host: &dyn AccountHost) -> Result<ChatPane> {
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("provider lead is unavailable"))?;
        let owned = matches!(provider, "codex" | "antigravity");
        let main = PaneIdentity::sample_with_host(&self.main, host, owned)?;
        let lead_id = PaneIdentity::sample_with_host(lead, host, owned)?;
        if provider == "antigravity" {
            let ok = self
                .binding_with_birth("orchestrator", "antigravity", lead, &lead_id.birth, host)
                .is_none_or(|b| {
                    b.verification == AccountVerification::Unverified
                        && b.account_id.is_none()
                        && !b.account_pinned
                        && b.home_digest.is_none()
                        && b.home_env_key.is_none()
                        && !b.has_codex_chat_home_identity()
                });
            require(ok, "provider account isolation is unproved for antigravity")?;
        }
        let codex = provider == "codex";
        let binding = codex
            .then(|| self.binding_with_birth("orchestrator", "codex", lead, &lead_id.birth, host))
            .flatten();
        let codex_source = codex.then(|| match (self.codex_chat_mark, binding.as_ref()) {
            (CodexChatMark::Bound, Some(binding)) if binding.has_codex_chat_home_identity() => {
                CodexChatSource::Bound
            }
            (CodexChatMark::Absent, Some(binding))
                if binding.verification == AccountVerification::Verified
                    && !binding.has_codex_chat_home_identity() =>
            {
                CodexChatSource::LegacyBound
            }
            (CodexChatMark::Absent, None) => CodexChatSource::Ambient,
            (CodexChatMark::Absent, Some(binding))
                if binding.verification == AccountVerification::UnpinnedAmbient =>
            {
                CodexChatSource::Ambient
            }
            _ => CodexChatSource::Unavailable,
        });
        let bound_account = binding
            .filter(|b| b.verification == AccountVerification::Verified)
            .and_then(|b| b.account_id.zip(b.home_digest));
        Ok(ChatPane {
            provider: provider.into(),
            repo: self.repo.canonicalize().map_err(Error::new)?,
            room: self.room.into(),
            attempt: self.attempt.clone(),
            owner: self.owner.clone(),
            session: self.session_proof.clone(),
            board: self.caller.clone(),
            main,
            lead: lead_id,
            bound_account,
            codex_source,
        })
    }

    fn codex_root(&self, pane: &ChatPane, host: &dyn AccountHost) -> Option<CodexChatRoot> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        let source = pane.codex_source?;
        if source == CodexChatSource::Bound {
            let lead = self.leads.get("codex")?;
            let anchor = self.binding_anchor("codex", lead, &pane.lead.birth);
            let proof = account_binding::resolve_codex_chat_home(host, &anchor)?;
            let (_, digest) = pane.bound_account.as_ref()?;
            return (account_binding::path_digest(proof.home()) == *digest)
                .then_some(CodexChatRoot::Bound(proof));
        }
        if source == CodexChatSource::Unavailable {
            return None;
        }
        let home = host.active_home("codex")?;
        let (home, root) = crate::native_children::open_ambient_codex_root(&home)?;
        if source == CodexChatSource::LegacyBound
            && pane.bound_account.as_ref()?.1 != account_binding::path_digest(&home)
        {
            return None;
        }
        Some(CodexChatRoot::Ambient(home, root))
    }

    fn has_live_report_scope(&self, report: &Value) -> bool {
        let main = &report["room_attempts"][&self.attempt];
        report["collection"]["ok"] == true
            && report["collection"]["pending"] != true
            && self.has_live_main_membership(&report["room"])
            && self.is_live_main(main)
            && !main["refs"]
                .get("panel_room_participant")
                .is_some_and(|member| member != &self.participant)
    }

    fn observe_codex_children(
        &self,
        pane: ChatPane,
        report: &Value,
        host: &dyn AccountHost,
    ) -> Option<(ChatPane, crate::native_children::ObservedChildren)> {
        if pane.codex_source != Some(CodexChatSource::Ambient)
            && !self.has_live_report_scope(report)
        {
            return None;
        }
        let root = self.codex_root(&pane, host)?;
        let terminal = self.capture_recent(&pane.lead.id).ok()?;
        let (home, fd) = root.parts();
        let mut observed =
            crate::native_children::observe_codex(&pane.repo, &terminal, home, fd, self.stop)?;
        if observed.rows.is_empty() || self.stop.load(Ordering::Acquire) {
            return None;
        }
        let fresh = self.reload().ok()?;
        let current_terminal = fresh.capture_recent(&pane.lead.id).ok()?;
        let current = fresh.chat_pane_with_host("codex", host).ok()?;
        if current != pane
            || !crate::native_children::same_codex_turn(&terminal, &current_terminal)
            || !root.same_root(&fresh.codex_root(&current, host)?)
            || self.stop.load(Ordering::Acquire)
        {
            return None;
        }
        strip_native_answers(&mut observed.rows);
        Some((pane, observed))
    }

    fn prove_chat(
        &self,
        provider: &str,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> Result<ChatPane> {
        crate::parent_only()?;
        self.prove_chat_with_host(provider, owner, attempts, self.chat_account_host())
    }

    fn prove_chat_with_host(
        &self,
        provider: &str,
        owner: Option<&str>,
        attempts: &AttemptCache,
        host: &dyn AccountHost,
    ) -> Result<ChatPane> {
        require(
            self.docked().is_none(),
            "native provider dock is already open",
        )?;
        self.require_board_pane_custody(owner)?;
        let room = commands::query(
            self.services,
            NativeCommand::Room,
            self.repo,
            &["show", "--id", self.room, "--exact", "--json"],
            self.stop,
        )?;
        require(
            self.has_live_main_membership(&room),
            "Main membership in this exact room could not be verified",
        )?;
        if !attempts
            .current_attempt(self.repo, &self.attempt, self.stop)
            .is_some_and(|main| self.is_live_main(&main))
        {
            let main = commands::query(
                self.services,
                NativeCommand::Events,
                self.repo,
                &["show", "--attempt", &self.attempt, "--json"],
                self.stop,
            )?;
            require(
                self.is_live_main(&main),
                "live Main attempt could not be verified",
            )?;
        }
        let pane = self.chat_pane_with_host(provider, host)?;
        if pane.codex_source == Some(CodexChatSource::LegacyBound)
            && let Some((_, ref bound_digest)) = pane.bound_account
        {
            require(
                host.active_home("codex")
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .map(|p| account_binding::path_digest(&p))
                    .as_deref()
                    == Some(bound_digest.as_str()),
                "Codex chat requires active home matching bound account",
            )?;
        }
        require(
            pane.codex_source != Some(CodexChatSource::Unavailable),
            "Codex chat account proof is unavailable",
        )?;
        Ok(pane)
    }

    fn has_live_main_membership(&self, room: &Value) -> bool {
        let mut mains = crate::rows(&room["participants"]).iter().filter(|row| {
            row["participant"] == self.participant
                || (row["role"] == "main" && row["joined"] == true)
        });
        room["id"] == self.room
            && room["closed"] == false
            && room["successor"].is_null()
            && mains.next().is_some_and(|row| {
                row["participant"] == self.participant
                    && row["role"] == "main"
                    && row["provider"] == self.owner
                    && row["joined"] == true
                    && row["terminal"] != true
                    && !row["state"].as_str().is_some_and(|state| {
                        oms_panel_core::state::TERMINAL_STATES.contains(&state)
                    })
                    && (row["parent"].is_null() || row["parent"] == "")
            })
            && mains.next().is_none()
    }

    fn is_live_main(&self, main: &Value) -> bool {
        main["attempt_id"] == self.attempt
            && main["provider"] == self.owner
            && main["tool"] == "panel-main"
            && main["refs"]["panel_role"] == "main"
            && main["refs"]["panel_room_id"] == self.room
            && oms_panel_core::state::main_participant(main) == self.participant
            && main["terminal"] == false
            && main["state"]
                .as_str()
                .is_some_and(|state| oms_panel_core::state::LIVE_STATES.contains(&state))
    }

    fn agy_pane_turn_matches(&self, pane: &ChatPane, turn: &str) -> bool {
        self.chat_pane("antigravity").ok().as_ref() == Some(pane)
            && self
                .capture_recent(&pane.lead.id)
                .ok()
                .as_deref()
                .and_then(crate::native_children::agy_current_turn)
                == Some(turn)
    }

    fn agy_chat_fenced_pre_check(
        &self,
        binding: &ChatBinding,
        pid: u32,
        birth: &str,
        turn: &str,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> Option<ChatPane> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        let pane = self.prove_chat("antigravity", owner, attempts).ok()?;
        (pane == binding.pane
            && pid == pane.lead.pid
            && birth == pane.lead.birth
            && self.agy_pane_turn_matches(&pane, turn))
        .then_some(pane)
    }

    fn agy_post_fence_current(
        &self,
        pane: &ChatPane,
        turn: &str,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> bool {
        !self.stop.load(Ordering::Acquire)
            && self.reload().ok().is_some_and(|fresh| {
                fresh
                    .prove_chat("antigravity", owner, attempts)
                    .ok()
                    .as_ref()
                    == Some(pane)
                    && fresh.agy_pane_turn_matches(pane, turn)
            })
            && !self.stop.load(Ordering::Acquire)
    }

    pub(crate) fn open_agy_chat(
        &self,
        binding: &ChatBinding,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> crate::agy_child_chat::AgyChatOpen {
        use crate::agy_child_chat::{AgyChatOpen, agy_open_selected_child_fenced};

        let NativeLocator::Antigravity(selected) = &binding.locator else {
            return AgyChatOpen::Unavailable;
        };
        let Some(pane) = self.agy_chat_fenced_pre_check(
            binding,
            selected.pid,
            &selected.birth,
            &selected.turn,
            owner,
            attempts,
        ) else {
            return AgyChatOpen::Unavailable;
        };
        agy_open_selected_child_fenced(&pane.repo, selected, || {
            self.agy_post_fence_current(&pane, &selected.turn, owner, attempts)
        })
    }

    pub(crate) fn recheck_open_agy_chat(
        &self,
        binding: &ChatBinding,
        locator: &crate::agy_child_chat::AgyChildChatLocator,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> bool {
        let NativeLocator::Antigravity(selected) = &binding.locator else {
            return false;
        };
        if selected.pid != locator.pid
            || selected.birth != locator.birth
            || selected.turn != locator.record.turn
            || crate::native_children::participant_id("antigravity", &locator.record.child_id)
                != selected.participant
        {
            return false;
        }
        let Some(pane) = self.agy_chat_fenced_pre_check(
            binding,
            locator.pid,
            &locator.birth,
            &locator.record.turn,
            owner,
            attempts,
        ) else {
            return false;
        };
        crate::agy_child_chat::agy_locator_still_current_fenced(&pane.repo, locator, || {
            self.agy_post_fence_current(&pane, &locator.record.turn, owner, attempts)
        })
    }

    pub(crate) fn open_chat(
        &self,
        binding: &ChatBinding,
        owner: Option<&str>,
        attempts: &AttemptCache,
    ) -> Option<String> {
        if matches!(binding.locator, NativeLocator::Antigravity(_)) {
            return match self.open_agy_chat(binding, owner, attempts) {
                #[cfg(target_os = "linux")]
                crate::agy_child_chat::AgyChatOpen::OwnChat { history, .. } => Some(history),
                crate::agy_child_chat::AgyChatOpen::Unbound
                | crate::agy_child_chat::AgyChatOpen::Unavailable => None,
            };
        }
        self.open_chat_checked_with(
            binding,
            owner,
            attempts,
            |repo, terminal, locator, root| {
                read_native_chat(repo, terminal, locator, root, self.stop)
            },
            |pid, repo| {
                crate::claude_child_chat::claude_current_children(pid, repo).map(|(turn, _)| turn)
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn open_chat_with(
        &self,
        binding: &ChatBinding,
        owner: Option<&str>,
        attempts: &AttemptCache,
        reader: impl FnOnce(&Path, &str, &NativeLocator) -> Option<String>,
    ) -> Option<String> {
        self.open_chat_checked(binding, owner, attempts, reader, |pid, repo| {
            crate::claude_child_chat::claude_current_children(pid, repo).map(|(turn, _)| turn)
        })
    }

    /// `current_turn` is the Claude lead's last managed Turn, a test seam.
    #[cfg(test)]
    pub(crate) fn open_chat_checked(
        &self,
        binding: &ChatBinding,
        owner: Option<&str>,
        attempts: &AttemptCache,
        reader: impl FnOnce(&Path, &str, &NativeLocator) -> Option<String>,
        current_turn: impl FnOnce(u32, &Path) -> Option<String>,
    ) -> Option<String> {
        self.open_chat_checked_with(
            binding,
            owner,
            attempts,
            |repo, terminal, locator, _| reader(repo, terminal, locator),
            current_turn,
        )
    }

    fn open_chat_checked_with(
        &self,
        binding: &ChatBinding,
        owner: Option<&str>,
        attempts: &AttemptCache,
        reader: impl FnOnce(&Path, &str, &NativeLocator, Option<&CodexChatRoot>) -> Option<String>,
        current_turn: impl FnOnce(u32, &Path) -> Option<String>,
    ) -> Option<String> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        let provider = match binding.locator {
            NativeLocator::Codex(_) => "codex",
            NativeLocator::Claude(_) => "claude",
            NativeLocator::Antigravity(_) => "antigravity",
        };
        let pane = self.prove_chat(provider, owner, attempts).ok()?;
        if pane != binding.pane {
            return None;
        }
        match &binding.locator {
            NativeLocator::Codex(codex) => {
                let terminal = self.capture_recent(&pane.lead.id).ok()?;
                if !crate::native_children::codex_chat_turn_matches(&terminal, codex)
                    || self.chat_pane(provider).ok().as_ref() != Some(&pane)
                {
                    return None;
                }
                let root = self.codex_root(&pane, self.chat_account_host())?;
                let history = reader(&pane.repo, &terminal, &binding.locator, Some(&root))?;
                if self.stop.load(Ordering::Acquire) {
                    return None;
                }
                let fresh = self.reload().ok()?;
                if fresh.prove_chat(provider, owner, attempts).ok()? != pane {
                    return None;
                }
                let terminal = fresh.capture_recent(&pane.lead.id).ok()?;
                let current = fresh.chat_pane(provider).ok()?;
                (crate::native_children::codex_chat_turn_matches(&terminal, codex)
                    && current == pane
                    && root.same_root(&fresh.codex_root(&current, fresh.chat_account_host())?)
                    && !self.stop.load(Ordering::Acquire))
                .then_some(history)
            }
            NativeLocator::Claude(claude) => {
                if claude.pid != pane.lead.pid
                    || self.chat_pane(provider).ok().as_ref() != Some(&pane)
                {
                    return None;
                }
                let history = reader(&pane.repo, "", &binding.locator, None)?;
                let fresh = self.reload().ok()?;
                if fresh.prove_chat(provider, owner, attempts).ok()? != pane {
                    return None;
                }
                (current_turn(claude.pid, &pane.repo).as_deref() == Some(claude.turn.as_str())
                    && fresh.chat_pane(provider).ok()? == pane)
                    .then_some(history)
            }
            NativeLocator::Antigravity(agy) => {
                if agy.pid != pane.lead.pid
                    || agy.birth != pane.lead.birth
                    || self.chat_pane(provider).ok().as_ref() != Some(&pane)
                {
                    return None;
                }
                let history = reader(&pane.repo, "", &binding.locator, None)?;
                if self.stop.load(Ordering::Acquire) {
                    return None;
                }
                let fresh = self.reload().ok()?;
                if fresh.prove_chat(provider, owner, attempts).ok()? != pane {
                    return None;
                }
                (current_turn(agy.pid, &pane.repo).as_deref() == Some(agy.turn.as_str())
                    && fresh.chat_pane(provider).ok()? == pane
                    && !self.stop.load(Ordering::Acquire))
                .then_some(history)
            }
        }
    }

    pub fn focus(&self, provider: &str) -> Result<()> {
        crate::parent_only()?;
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("provider session is not connected to this Main"))?;
        self.tmux(&[
            "select-window",
            "-t",
            &format!("{}:{}", self.session, lead.window),
        ])?;
        self.tmux(&["select-pane", "-t", &lead.id])?;
        Ok(())
    }

    pub(crate) fn docked(&self) -> Option<&'static str> {
        PROVIDERS.into_iter().find(|provider| {
            self.leads
                .get(*provider)
                .is_some_and(|pane| pane.window == self.main.window)
        })
    }

    #[cfg(test)]
    fn require_main(&self, owner: Option<&str>) -> Result<Value> {
        self.require_main_with_cache(owner, &AttemptCache::default())
    }

    fn require_main_with_cache(
        &self,
        owner: Option<&str>,
        attempts_cache: &AttemptCache,
    ) -> Result<Value> {
        require(
            self.caller == self.main.id
                && std::env::var("OMS_PANEL_MAIN_ATTEMPT").as_deref() == Ok(&self.attempt)
                && std::env::var("OMS_ROOM_PARTICIPANT").as_deref() == Ok(&self.participant)
                && owner.is_none_or(|owner| owner == self.owner),
            "provider sessions are managed by the room's Main",
        )?;
        self.require_membership()?;
        let attempts = attempts_cache.attempts(self.repo, self.services, self.stop)?;
        require(
            std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_none()
                && crate::rows(&attempts).iter().any(|row| {
                    row["attempt_id"] == self.attempt
                        && row["tool"] == "panel-main"
                        && row["terminal"] == false
                        && row["refs"]["panel_room_id"] == self.room
                        && oms_panel_core::state::main_participant(row) == self.participant
                        && row["refs"]["panel_native_scheduling"] == "unmanaged"
                }),
            "native provider scheduling requires a verified Main outside a managed campaign",
        )?;
        Ok(attempts)
    }

    fn require_membership(&self) -> Result<()> {
        let room = commands::query(
            self.services,
            NativeCommand::Room,
            self.repo,
            &["show", "--id", self.room, "--exact", "--json"],
            self.stop,
        )?;
        require(
            self.has_live_main_membership(&room),
            "Main membership in this room could not be verified",
        )
    }

    fn require_board(&self, owner: Option<&str>) -> Result<()> {
        self.require_board_custody(owner)?;
        self.require_membership()
    }

    fn require_board_custody(&self, owner: Option<&str>) -> Result<()> {
        crate::parent_only()?;
        self.require_board_pane_custody(owner)
    }

    fn require_board_pane_custody(&self, owner: Option<&str>) -> Result<()> {
        let board = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &self.caller,
            "#{session_id}\t#{window_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{pane_in_mode}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room}\t#{@oms_panel_repo}",
        ])?;
        require(
            self.caller != self.main.id
                && owner.is_none_or(|owner| owner == self.owner)
                && board.split('\t').eq([
                    self.session.as_str(),
                    self.main.window.as_str(),
                    &std::process::id().to_string(),
                    "0",
                    "1",
                    "0",
                    "1",
                    self.attempt.as_str(),
                    self.room,
                    self.repo.to_str().unwrap_or_default(),
                ]),
            "native chat requires the managed board in the active Main window",
        )
    }

    /// Move the existing provider PTY between its own window and the bottom of the board.
    pub(crate) fn toggle_dock(
        &self,
        provider: Option<&str>,
        preferred_rows: usize,
        owner: Option<&str>,
    ) -> Result<Option<&'static str>> {
        crate::parent_only()?;
        let store = Store::open(self.repo)?;
        let tx = store.transaction("panel-provider-sessions", Duration::from_secs(3))?;
        let fresh = self.reload()?;
        require(
            self.main == fresh.main
                && self.leads == fresh.leads
                && self.session_proof == fresh.session_proof
                && self.attempt == fresh.attempt
                && self.participant == fresh.participant
                && self.owner == fresh.owner,
            "provider panes changed; select the provider again",
        )?;
        fresh.require_board(owner)?;
        let current = self.docked();
        let next = provider.filter(|provider| Some(*provider) != current);
        let lead = next
            .map(|provider| {
                self.leads
                    .get(provider)
                    .ok_or_else(|| Error::new("provider session is unavailable"))
            })
            .transpose()?;
        if current.is_none() && lead.is_none() {
            return Ok(None);
        }
        let remembered = if current.is_none() {
            self.tmux(&["show-options", "-pqv", "-t", &self.caller, NATIVE_DOCK_ROWS])
                .ok()
        } else {
            None
        };
        let current_pane = current.map(|provider| &self.leads[provider]);
        let window = self.native_dock_window(current_pane, lead)?;
        let collapsed = window.collapse();
        let size = if lead.is_some() {
            let (destination, rows) = if let Some(dock) = window.dock {
                let collapsed = collapsed.as_ref().ok_or_else(|| {
                    Error::new("native chat needs a dock immediately below the managed board")
                })?;
                (collapsed, dock.height)
            } else {
                let scope = self.native_dock_preference_scope(&window);
                let rows = remembered
                    .as_deref()
                    .and_then(|value| value.strip_prefix(&scope)?.parse::<u16>().ok())
                    .filter(|&rows| rows != 0)
                    .map_or(preferred_rows, usize::from);
                (&window, rows)
            };
            Some(
                crate::layout::dock_join_rows(
                    destination.board.window_size.0,
                    destination.board.window_size.1,
                    destination.board.height,
                    rows,
                )
                .ok_or_else(|| Error::new("make the panel larger to dock native chat"))?
                .to_string(),
            )
        } else {
            None
        };
        if let Some((current, pane)) = current.zip(current_pane) {
            tx.check()?;
            self.tmux(&[
                "break-pane",
                "-d",
                "-s",
                &pane.id,
                "-n",
                &format!("{} Orchestrator", provider_name(current)),
                ";",
                "set-option",
                "-pqu",
                "-t",
                &self.caller,
                NATIVE_DOCK_ROWS,
            ])?;
            self.tag_provider(&pane.id, current, "-w")?;
            if next.is_none()
                && let (Some(expected), Some(dock)) = (&collapsed, window.dock)
                && self
                    .native_dock_window(None, None)
                    .is_ok_and(|actual| actual == *expected)
            {
                let scope = self.native_dock_preference_scope(expected);
                let rows = dock.height;
                self.tmux(&[
                    "set-option",
                    "-p",
                    "-t",
                    &self.caller,
                    NATIVE_DOCK_ROWS,
                    &format!("{scope}{rows}"),
                ])?;
            }
        }
        if let (Some(provider), Some(lead), Some(size)) = (next, lead, size) {
            tx.check()?;
            if let Some(expected) = &collapsed {
                require(
                    self.native_dock_window(None, Some(lead))? == *expected,
                    "native chat pane layout changed; select the provider again",
                )?;
            }
            self.tag_provider(&lead.id, provider, "-p")?;
            self.tmux(&[
                "join-pane",
                "-d",
                "-v",
                "-l",
                &size,
                "-s",
                &lead.id,
                "-t",
                &self.caller,
                ";",
                "set-option",
                "-pqu",
                "-t",
                &self.caller,
                NATIVE_DOCK_ROWS,
            ])?;
            self.tmux(&["select-pane", "-t", &lead.id])?;
        }
        Ok(PROVIDERS
            .into_iter()
            .find(|provider| Some(*provider) == next))
    }

    fn native_dock_preference_scope(&self, window: &NativeDockWindow) -> String {
        let (width, height) = window.board.window_size;
        let (board, main) = (&window.board, &window.main);
        format!(
            "{}:{}:{}:{width}x{height}:{}x{}+{}+{}:{}x{}+{}+{}:",
            self.session,
            self.main.window,
            self.attempt,
            board.width,
            board.height,
            board.left,
            board.top,
            main.width,
            main.height,
            main.left,
            main.top,
        )
    }

    fn native_dock_window(
        &self,
        current: Option<&Pane>,
        incoming: Option<&Pane>,
    ) -> Result<NativeDockWindow> {
        let mut args = vec!["list-panes", "-t", &self.caller, "-F", NATIVE_DOCK_GEOMETRY];
        if let Some(pane) = incoming {
            args.extend([
                ";",
                "display-message",
                "-p",
                "-t",
                &pane.id,
                NATIVE_DOCK_GEOMETRY,
            ]);
        }
        let sample = self.tmux(&args)?;
        let count = 2 + usize::from(current.is_some());
        let mut rows: Vec<_> = sample.lines().collect();
        require(
            rows.len() == count + usize::from(incoming.is_some()),
            "native chat needs the managed Board and Main layout",
        )?;
        if let Some(pane) = incoming {
            let sample = rows
                .pop()
                .ok_or_else(|| Error::new("native chat target is unavailable"))?;
            self.native_dock_geometry(sample, &pane.id, &pane.window, pane.pid, "1", None)?;
        }
        let count = count.to_string();
        let read = |id: &str, pid, attempt| {
            let mut matching = rows.iter().filter(|row| row.split('\t').nth(2) == Some(id));
            let row = matching
                .next()
                .ok_or_else(|| Error::new("native chat pane is unavailable"))?;
            require(
                matching.next().is_none(),
                "native chat pane layout is ambiguous",
            )?;
            self.native_dock_geometry(row, id, &self.main.window, pid, &count, attempt)
        };
        let (board, main, dock) = (
            read(
                &self.caller,
                std::process::id(),
                Some(self.attempt.as_str()),
            )?,
            read(&self.main.id, self.main.pid, Some(self.attempt.as_str()))?,
            current
                .map(|pane| read(&pane.id, pane.pid, None))
                .transpose()?,
        );
        require(
            board.window_size == main.window_size
                && dock.is_none_or(|dock| dock.window_size == board.window_size),
            "native chat pane layout changed; select the provider again",
        )?;
        Ok(NativeDockWindow { board, main, dock })
    }

    fn native_dock_geometry(
        &self,
        sample: &str,
        pane: &str,
        window: &str,
        pid: u32,
        panes: &str,
        attempt: Option<&str>,
    ) -> Result<NativeDockGeometry> {
        let fields: Vec<_> = sample.split('\t').collect();
        let [
            s,
            w,
            p,
            process,
            "0",
            "0",
            count,
            window_width,
            window_height,
            left,
            top,
            width,
            height,
            observed_attempt,
            ".",
        ] = fields.as_slice()
        else {
            return Err(Error::new(
                "native chat needs an unzoomed managed pane layout",
            ));
        };
        require(
            *s == self.session
                && *w == window
                && *p == pane
                && process.parse() == Ok(pid)
                && *count == panes
                && attempt.is_none_or(|attempt| *observed_attempt == attempt),
            "native chat pane layout changed; select the provider again",
        )?;
        let cell = |value: &str| value.parse::<u16>().map(usize::from).map_err(Error::new);
        let (window_width, window_height) = (cell(window_width)?, cell(window_height)?);
        let (left, top, width, height) = (cell(left)?, cell(top)?, cell(width)?, cell(height)?);
        require(
            width != 0
                && height != 0
                && left + width <= window_width
                && top + height <= window_height,
            "native chat pane geometry is invalid",
        )?;
        Ok(NativeDockGeometry {
            window_size: (window_width, window_height),
            left,
            top,
            width,
            height,
        })
    }

    fn tag_provider(&self, id: &str, provider: &str, scope: &str) -> Result<()> {
        // The provider tag admits the pane. Keep it last in tmux's fail-stop sequence.
        self.tmux(&[
            "set-option",
            scope,
            "-t",
            id,
            "@oms_panel_room",
            self.room,
            ";",
            "set-option",
            scope,
            "-t",
            id,
            "@oms_provider_parent",
            &self.attempt,
            ";",
            "set-option",
            scope,
            "-t",
            id,
            "@oms_provider_session",
            provider,
        ])?;
        Ok(())
    }

    fn tmux_env_lines(&self) -> Result<Vec<String>> {
        let global = self.tmux(&["show-environment", "-g"])?;
        let session = self.tmux(&["show-environment", "-t", &self.session])?;
        Ok(global
            .lines()
            .chain(session.lines())
            .map(str::to_owned)
            .collect())
    }

    fn open_with_host(
        &self,
        provider: &str,
        account: LeadAccount<'_>,
        launch: LeadLaunch<'_>,
        host: &dyn AccountHost,
    ) -> Result<(String, AccountSelectionReceipt)> {
        let model = launch.model(provider)?;
        let permissions = launch
            .permissions
            .map_or_else(MainPermissions::from_env, Ok)?;
        let command = orchestrator_command(provider, model, launch.effort, permissions)?;
        let env_lines = self.tmux_env_lines()?;
        let env_refs: Vec<&str> = env_lines.iter().map(String::as_str).collect();
        let receipt = match account {
            LeadAccount::Default(requested_account) => account_binding::resolve_selection(
                host,
                SelectionTarget {
                    role: "orchestrator",
                    provider,
                    model,
                },
                requested_account,
                &env_refs,
                oms_panel_core::timestamp(),
            )?,
            LeadAccount::Spread(attempts) => account_binding::resolve_claude_spread(
                host,
                self.repo,
                model,
                &claude_worker_load(attempts),
                &env_refs,
            )?,
        };
        if self.live_providers.contains(provider)
            || (provider == "codex" && self.alternate.is_err())
        {
            let alternate = (provider == "codex")
                .then(|| self.only_verified_alternate(host))
                .flatten()
                .ok_or_else(|| Error::new("provider already has a live pane in this panel"))?;
            let home = account_binding::selected_codex_home(host, &receipt).ok_or_else(|| {
                Error::new("a live Codex alternate requires a verified primary account home")
            })?;
            alternate.require_distinct(receipt.account_id.as_deref(), &home)?;
        }
        let created = self.spawn_provider_window(
            &format!("{} Orchestrator", provider_name(provider)),
            PROVIDER_SPAWN_FORMAT,
            command,
            &env_lines,
            &receipt,
        )?;
        let (id, pid_raw) = created
            .split_once('\t')
            .ok_or_else(|| Error::new("provider pane could not be verified"))?;
        require(
            commands::valid_target(id, '%'),
            "provider pane could not be verified",
        )?;
        let finalize = || -> Result<()> {
            let pane_pid: u32 = pid_raw
                .parse()
                .map_err(|_| Error::new("provider pane pid could not be verified"))?;
            if launch.model.is_some() && receipt.verification == AccountVerification::Verified {
                // Reuse the requested model for later account eligibility checks.
                // This records launch intent, not provider acceptance.
                self.tmux(&[
                    "set-option",
                    "-p",
                    "-t",
                    id,
                    REQUESTED_PROVIDER_MODEL,
                    model,
                ])?;
            }
            if provider == "codex" && receipt.verification == AccountVerification::Verified {
                self.tmux(&["set-option", "-p", "-t", id, "@oms_codex_chat_bound", "1"])?;
            }
            if provider == "claude" && receipt.verification == AccountVerification::Verified {
                let mark = receipt.claude_auto_observation.as_ref().map_or_else(
                    || {
                        if receipt.account_pinned {
                            "1".into()
                        } else {
                            "auto".into()
                        }
                    },
                    |launch| launch.marker(),
                );
                self.tmux(&[
                    "set-option",
                    "-p",
                    "-t",
                    id,
                    "@oms_claude_quota_bound",
                    &mark,
                ])?;
            }
            self.tag_provider(id, provider, "-w")?;
            let owner = host.process_owner(pane_pid)?;
            require(
                owner.uid == host.uid(),
                "provider pane owner does not match current user",
            )?;
            let anchor = BindingAnchor {
                repo: self.repo,
                room: self.room,
                main_attempt: &self.attempt,
                main_owner: &self.owner,
                session: &self.session_proof,
                provider,
                pane_id: id,
                pid: pane_pid,
                birth: &owner.birth,
            };
            account_binding::save_binding(host, "orchestrator", &anchor, &receipt)
        };
        self.finish_new_provider(id, finalize())?;
        let _ = self.tmux(&["rename-window", "-t", &self.main.window, MAIN_WINDOW]);
        let _ = self.tmux(&["set-option", "-t", &self.session, "mouse", "on"]);
        Ok((id.to_owned(), receipt))
    }

    fn spawn_provider_window(
        &self,
        name: &str,
        spawn_format: &str,
        command: Vec<OsString>,
        env_lines: &[String],
        receipt: &AccountSelectionReceipt,
    ) -> Result<String> {
        let mut strip: BTreeSet<&str> = env_lines
            .iter()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .filter(|key| inherited_authority(key))
            .collect();
        strip.insert("TMUX");
        strip.insert("TMUX_PANE");
        strip.insert(crate::claude_lead_usage::AUTO_OBSERVATION_SWITCH);
        if receipt.claude_auto_observation.is_some() {
            strip.insert(crate::claude_lead_usage::OBSERVATION_SWITCH);
        }
        for key in receipt.scrub_env {
            strip.insert(key);
        }
        if let Some((env_key, _)) = &receipt.home_env {
            strip.insert(env_key);
        }
        let mut argv = commands::strings(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            spawn_format,
            "-t",
            &format!("{}:", self.session),
            "-n",
            name,
            "-c",
        ]);
        argv.push(self.repo.into());
        argv.push("env".into());
        for key in strip {
            argv.extend(commands::strings(&["-u", key]));
        }
        // Keep the OMS child markers even when the provider runs with full native permissions.
        argv.extend(commands::strings(&[
            "OMS_HARNESS_CHILD=1",
            "OMS_HARNESS_DELEGATE_DEPTH=1",
        ]));
        if let Some((env_key, canon_home)) = &receipt.home_env {
            let mut assign = OsString::from(env_key);
            assign.push("=");
            assign.push(canon_home.as_os_str());
            argv.push(assign);
        }
        if let Some(identity) = &receipt.claude_observation_identity {
            argv.push(
                format!(
                    "{}={identity}",
                    crate::claude_lead_usage::OBSERVATION_SWITCH
                )
                .into(),
            );
        }
        if let Some(launch) = &receipt.claude_auto_observation {
            argv.push(
                format!(
                    "{}={}",
                    crate::claude_lead_usage::AUTO_OBSERVATION_SWITCH,
                    launch.switch_value()
                )
                .into(),
            );
        }
        argv.extend(command);
        terminals::tmux(self.services, self.repo, &argv, self.stop)
    }

    fn kill_new_provider(&self, id: &str, stop: &AtomicBool) -> Result<()> {
        terminals::tmux(
            self.services,
            self.repo,
            &commands::strings(&["kill-pane", "-t", id]),
            stop,
        )?;
        Ok(())
    }

    fn finish_new_provider(&self, id: &str, result: Result<()>) -> Result<()> {
        if let Err(error) = result {
            self.kill_new_provider(id, &AtomicBool::new(false))
                .map_err(|cleanup| {
                    Error::new(format!(
                        "{error}; new provider pane {id} cleanup failed: {cleanup}"
                    ))
                })?;
            return Err(error);
        }
        Ok(())
    }

    fn alternate_main(
        &self,
        caller: &AlternateCaller,
        host: &dyn AccountHost,
    ) -> Result<PaneIdentity> {
        require(
            self.caller == self.main.id
                && !caller.campaign
                && caller.room == self.room
                && caller.owner == self.owner
                && caller.attempt == self.attempt
                && caller.participant == self.participant
                && caller.repo.is_absolute()
                && caller.repo.canonicalize().map_err(Error::new)?
                    == self.repo.canonicalize().map_err(Error::new)?,
            "the Codex alternate lead requires the room's current Main owner",
        )?;
        PaneIdentity::sample_with_host(&self.main, host, true)
    }

    fn alternate_slot(&self) -> Result<Option<&Pane>> {
        let alternate = self.alternate.as_ref().map_err(Clone::clone)?.as_ref();
        require(
            self.live_codex_panes == 1 + usize::from(alternate.is_some()),
            "the Codex alternate lead requires one unambiguous primary lead",
        )?;
        Ok(alternate)
    }

    fn alternate_primary(
        &self,
        main: &PaneIdentity,
        host: &dyn AccountHost,
    ) -> Result<BoundCodexPane> {
        let unavailable = || Error::new("the primary Codex lead has no verified account home");
        let primary = self.leads.get("codex").ok_or_else(unavailable)?;
        require(
            self.codex_chat_mark == CodexChatMark::Bound && primary.pid != main.pid,
            "the primary Codex lead has no verified account home",
        )?;
        let pane = PaneIdentity::sample_with_host(primary, host, true)?;
        let anchor = self.binding_anchor("codex", primary, &pane.birth);
        let receipt = account_binding::load_verified_binding(host, "orchestrator", &anchor)
            .ok_or_else(unavailable)?;
        let home = account_binding::bound_codex_home(host, &receipt).ok_or_else(unavailable)?;
        require(
            PaneIdentity::sample_with_host(primary, host, true)? == pane,
            "the primary Codex pane changed during account verification",
        )?;
        Ok(BoundCodexPane {
            pane,
            receipt,
            home,
        })
    }

    fn alternate_scope(main: &PaneIdentity, pane: &PaneIdentity) -> AlternateLeadScope {
        AlternateLeadScope {
            main_pane: main.id.clone(),
            main_pid: main.pid,
            main_birth: main.birth.clone(),
            window: pane.window.clone(),
        }
    }

    fn require_local_alternate_slot(&self, pane: &PaneIdentity) -> Result<()> {
        let signature = alternate_pane_signature(&self.session, pane);
        require(
            self.tmux(&[
                "display-message",
                "-p",
                "-t",
                &pane.id,
                ALTERNATE_PANE_FORMAT,
                ";",
                "show-options",
                "-pqv",
                "-t",
                &pane.id,
                "@oms_provider_parent",
            ])? == format!("{signature}\n{ALTERNATE_LEAD_TAG}"),
            "the Codex alternate lead slot changed or is not set on this pane",
        )
    }

    fn only_verified_alternate(&self, host: &dyn AccountHost) -> Option<BoundCodexPane> {
        if self.live_codex_panes != 1 {
            return None;
        }
        let pane = self.alternate.as_ref().ok()?.as_ref()?;
        let main = PaneIdentity::sample_with_host(&self.main, host, true).ok()?;
        let alternate = self.alternate_binding(&main, pane, host).ok()?;
        self.require_local_alternate_slot(&alternate.pane).ok()?;
        Some(alternate)
    }

    fn prove_alternate_binding(
        &self,
        main: &PaneIdentity,
        primary: &BoundCodexPane,
        host: &dyn AccountHost,
    ) -> Result<BoundCodexPane> {
        let pane = self
            .alternate_slot()?
            .ok_or_else(|| Error::new("the Codex alternate lead is not connected"))?;
        let selected = self.alternate_binding(main, pane, host)?;
        require(
            selected.pane.pid != primary.pane.pid,
            "the Codex alternate lead has a conflicting process identity",
        )?;
        primary.require_distinct(selected.receipt.account_id.as_deref(), &selected.home)?;
        Ok(selected)
    }

    fn alternate_binding(
        &self,
        main: &PaneIdentity,
        pane: &Pane,
        host: &dyn AccountHost,
    ) -> Result<BoundCodexPane> {
        let identity = PaneIdentity::sample_with_host(pane, host, true)?;
        require(
            identity.pid != main.pid,
            "the Codex alternate lead has a conflicting process identity",
        )?;
        let anchor = self.binding_anchor("codex", pane, &identity.birth);
        let scope = Self::alternate_scope(main, &identity);
        let unavailable = || Error::new("the Codex alternate lead has no verified account binding");
        let receipt = account_binding::load_alternate_binding(host, &anchor, &scope)
            .ok_or_else(unavailable)?;
        let home = account_binding::bound_codex_home(host, &receipt).ok_or_else(unavailable)?;
        require(
            PaneIdentity::sample_with_host(pane, host, true)? == identity,
            "the Codex alternate pane changed during account verification",
        )?;
        Ok(BoundCodexPane {
            pane: identity,
            receipt,
            home,
        })
    }

    fn verify_current_alternate_main(
        &self,
        original: &Self,
        main: &PaneIdentity,
        caller: &AlternateCaller,
        host: &dyn AccountHost,
    ) -> Result<()> {
        require(
            self.session_proof == original.session_proof
                && self.alternate_main(caller, host)? == *main,
            "the tmux session or room Main changed while verifying the Codex alternate lead",
        )
    }

    fn focus_codex_alternate(
        &self,
        caller: &AlternateCaller,
        host: &dyn AccountHost,
        tx: &Transaction<'_>,
    ) -> Result<Value> {
        let main = self.alternate_main(caller, host)?;
        let primary = self.alternate_primary(&main, host)?;
        let selected = self.prove_alternate_binding(&main, &primary, host)?;
        self.require_exact_main_membership(self.repo)?;
        let fresh = self.reload()?;
        fresh.verify_current_alternate_main(self, &main, caller, host)?;
        let current_primary = fresh.alternate_primary(&main, host)?;
        let current = fresh.prove_alternate_binding(&main, &current_primary, host)?;
        require(
            primary.same_custody(&current_primary) && selected.same_custody(&current),
            "the Codex lead bindings changed before focus",
        )?;
        fresh.require_local_alternate_slot(&current.pane)?;
        tx.check()?;
        let window = format!("{}:{}", fresh.session, current.pane.window);
        fresh.tmux(&[
            "select-pane",
            "-t",
            &format!("{window}.{}", current.pane.id),
            ";",
            "select-window",
            "-t",
            &window,
        ])?;
        Ok(alternate_result("focused", current.receipt.account_id))
    }

    fn launch_codex_alternate(
        &self,
        caller: &AlternateCaller,
        alias: &str,
        launch: LeadLaunch<'_>,
        host: &dyn AccountHost,
        tx: &Transaction<'_>,
    ) -> Result<Value> {
        let model = launch.model("codex")?;
        let main = self.alternate_main(caller, host)?;
        require(
            self.alternate_slot()?.is_none(),
            "the Codex alternate lead slot is already occupied",
        )?;
        let primary = self.alternate_primary(&main, host)?;
        let record = self.require_codex_assignment_main(self.repo, &AttemptCache::default())?;
        let permissions = lead_permissions(&record)?;
        let command = orchestrator_command("codex", model, launch.effort, permissions)?;
        let env = self.tmux_env_lines()?;
        let env_refs: Vec<&str> = env.iter().map(String::as_str).collect();
        let receipt =
            account_binding::resolve_codex_alternate_selection(host, alias, model, &env_refs)?;
        let selected_home = || {
            account_binding::selected_codex_home(host, &receipt)
                .ok_or_else(|| Error::new("the Codex alternate account home could not be verified"))
        };
        let home = selected_home()?;
        primary.require_distinct(receipt.account_id.as_deref(), &home)?;

        self.require_exact_main_membership(self.repo)?;
        let mut pane_windows = Vec::new();
        let fresh = self.reload_with_pane_census(Some(&mut pane_windows))?;
        fresh.verify_current_alternate_main(self, &main, caller, host)?;
        require(
            fresh.alternate_slot()?.is_none(),
            "the Codex alternate lead slot is already occupied",
        )?;
        let fresh_primary = fresh.alternate_primary(&main, host)?;
        let fresh_home = selected_home()?;
        require(
            primary.same_custody(&fresh_primary) && home.same_directory(&fresh_home),
            "the Codex account homes changed before launch",
        )?;
        tx.check()?;
        let unverified = || Error {
            publication_uncertain: true,
            ..Error::new(
                "the new Codex alternate pane could not be verified. A pane may have been created",
            )
        };
        let created = fresh
            .spawn_provider_window(
                "Codex Alternate",
                ALTERNATE_SPAWN_FORMAT,
                command,
                &env,
                &receipt,
            )
            .map_err(|_| unverified())?;
        let fields: Vec<_> = created.split('\t').collect();
        let [id, pid_field, window, session] = fields.as_slice() else {
            return Err(unverified());
        };
        if !pid_field.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(unverified());
        }
        let pid: u32 = pid_field.parse().map_err(|_| unverified())?;
        if !commands::valid_target(id, '%')
            || !commands::valid_target(window, '@')
            || *session != fresh.session
            || pid == 0
            || pid == main.pid
            || pid == fresh_primary.pane.pid
            || pane_windows.iter().any(|(existing_window, existing_pane)| {
                existing_pane == id || existing_window == window
            })
        {
            return Err(unverified());
        }
        let new = Pane {
            window: (*window).into(),
            id: (*id).into(),
            pid,
            active: false,
        };
        let owner = PaneIdentity::sample_with_host(&new, host, true).map_err(|_| unverified())?;
        let result = (|| -> Result<()> {
            let post = fresh.reload()?;
            post.verify_current_alternate_main(&fresh, &main, caller, host)?;
            require(
                post.alternate_slot()?.is_none(),
                "the Codex alternate lead slot changed during launch",
            )?;
            post.verify_new_alternate(&owner, host, self.stop)?;
            tx.check()?;
            // The window is classed as a provider by Main lifecycle scans. The pane-local
            // sentinel is disjoint from Main attempt IDs and is never a normal lead tag.
            let target = format!("{}:{}.{}", post.session, new.window, new.id);
            post.tmux(&[
                "set-option",
                "-p",
                "-t",
                &target,
                "@oms_provider_parent",
                ALTERNATE_LEAD_TAG,
                ";",
                "set-option",
                "-w",
                "-t",
                &target,
                "@oms_provider_session",
                "codex",
            ])?;
            post.require_exact_main_membership(self.repo)?;
            let installed = post.reload()?;
            installed.verify_current_alternate_main(&fresh, &main, caller, host)?;
            let pane = installed
                .alternate_slot()?
                .filter(|pane| {
                    pane.id == new.id && pane.pid == new.pid && pane.window == new.window
                })
                .ok_or_else(|| {
                    Error::new("the new Codex alternate pane scope could not be verified")
                })?;
            let identity = PaneIdentity::sample_with_host(pane, host, true)?;
            let current_primary = installed.alternate_primary(&main, host)?;
            require(
                identity.birth == owner.birth
                    && primary.same_custody(&current_primary)
                    && home.same_directory(&selected_home()?),
                "the Codex lead identities changed during launch",
            )?;
            installed.require_local_alternate_slot(&identity)?;
            tx.check()?;
            account_binding::save_alternate_binding(
                host,
                &installed.binding_anchor("codex", pane, &identity.birth),
                &receipt,
                &Self::alternate_scope(&main, &identity),
            )
        })();
        if let Err(error) = result {
            let cleanup = AtomicBool::new(false);
            let cleaned = (|| -> Result<()> {
                require(
                    terminals::provider_session_proof(
                        self.services,
                        self.repo,
                        &fresh.session,
                        &cleanup,
                    )? == fresh.session_proof,
                    "the tmux session changed before rollback",
                )?;
                fresh.verify_new_alternate(&owner, host, &cleanup)?;
                fresh.kill_new_provider(
                    &format!("{}:{}.{}", fresh.session, new.window, new.id),
                    &cleanup,
                )
            })();
            return Err(match cleaned {
                Ok(()) => error,
                Err(cleanup) => Error {
                    publication_uncertain: true,
                    ..Error::new(format!(
                        "{error}. The new Codex alternate pane could not be safely rolled back and may still exist. {cleanup}"
                    ))
                },
            });
        }
        Ok(alternate_result("started", receipt.account_id))
    }

    fn verify_new_alternate(
        &self,
        expected: &PaneIdentity,
        host: &dyn AccountHost,
        stop: &AtomicBool,
    ) -> Result<()> {
        let signature = terminals::tmux(
            self.services,
            self.repo,
            &commands::strings(&[
                "display-message",
                "-p",
                "-t",
                &expected.id,
                ALTERNATE_PANE_FORMAT,
            ]),
            stop,
        )?;
        let owner = host.process_owner(expected.pid)?;
        require(
            signature == alternate_pane_signature(&self.session, expected)
                && owner.uid == host.uid()
                && owner.pid == expected.pid
                && owner.birth == expected.birth,
            "the new Codex alternate pane generation changed",
        )
    }

    fn codex_assignment_custody(&self, host: &dyn AccountHost) -> Result<CodexAssignment> {
        self.codex_assignment_custody_from(host, None)
    }

    fn codex_assignment_custody_from(
        &self,
        host: &dyn AccountHost,
        declared: Option<&Value>,
    ) -> Result<CodexAssignment> {
        let lead = self
            .leads
            .get("codex")
            .ok_or_else(|| Error::new("Codex lead is unavailable"))?;
        let main = PaneIdentity::sample_with_host(&self.main, host, true)?;
        let lead_id = PaneIdentity::sample_with_host(lead, host, true)?;
        let direct = self.binding_anchor("codex", lead, &lead_id.birth);
        let inherited = if let Some(declared) = declared {
            Some(declared.clone())
        } else if self.inherited_codex_parent.is_some()
            || (self.attempt != self.participant
                && account_binding::load_verified_binding(host, "orchestrator", &direct).is_none())
        {
            let origin = native_succession::account_origin(self, &main, &lead_id)?;
            require(
                self.inherited_codex_parent.is_none() || origin.is_some(),
                "previous Main's Codex Lead requires explicit native adoption",
            )?;
            origin
        } else {
            None
        };
        let inherited = inherited
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<PersistedAccountBinding>)
            .transpose()
            .map_err(Error::new)?;
        let anchor = if let Some(binding) = &inherited {
            require(
                binding.session == self.session_proof
                    && binding.room == self.room
                    && binding.provider == "codex"
                    && binding.pane_id == lead_id.id
                    && binding.pid == lead_id.pid
                    && binding.birth == lead_id.birth,
                "inherited native account custody changed",
            )?;
            BindingAnchor {
                main_attempt: &binding.main_attempt,
                main_owner: &binding.main_owner,
                session: &binding.session,
                ..direct
            }
        } else {
            direct
        };
        let binding = account_binding::load_verified_binding(host, "orchestrator", &anchor);
        require(
            inherited
                .as_ref()
                .is_none_or(|expected| binding.as_ref() == Some(expected)),
            "original native account binding is missing or changed",
        )?;
        let unavailable = || Error::new("Codex assignment account custody could not be verified");
        let root = match (self.codex_chat_mark, binding.as_ref()) {
            (CodexChatMark::Absent, None) => None,
            (CodexChatMark::Absent, Some(binding))
                if binding.verification != AccountVerification::Verified =>
            {
                None
            }
            (CodexChatMark::Bound, Some(binding)) => {
                let root = account_binding::resolve_codex_chat_home(host, &anchor)
                    .ok_or_else(unavailable)?;
                require(
                    binding.home_digest.as_deref()
                        == Some(account_binding::path_digest(root.home()).as_str()),
                    "Codex assignment account custody changed",
                )?;
                Some(CodexChatRoot::Bound(root))
            }
            (CodexChatMark::Absent, Some(binding)) if !binding.has_codex_chat_home_identity() => {
                Some(legacy_assignment_root(host, binding).ok_or_else(unavailable)?)
            }
            _ => return Err(unavailable()),
        };
        if root.is_some() {
            require(
                account_binding::load_verified_binding(host, "orchestrator", &anchor) == binding,
                "Codex assignment account custody changed",
            )?;
        }
        Ok(CodexAssignment {
            repo: self.repo.canonicalize().map_err(Error::new)?,
            main,
            lead: lead_id,
            binding,
            root,
        })
    }

    fn require_codex_assignment_main(&self, repo: &Path, attempts: &AttemptCache) -> Result<Value> {
        let main = attempts.resolve_attempt(repo, &self.attempt, self.services, self.stop)?;
        self.require_live_codex_assignment_main(main.as_ref())?;
        self.require_exact_main_membership(repo)?;
        main.ok_or_else(|| Error::new("live Main attempt unavailable"))
    }

    fn require_live_codex_assignment_main(&self, main: Option<&Value>) -> Result<()> {
        require(
            main.is_some_and(|main| {
                self.is_live_main(main)
                    && main["refs"]["panel_native_scheduling"] == "unmanaged"
                    && main["refs"]
                        .get("panel_room_participant")
                        .is_none_or(|member| member == &self.participant)
            }),
            "live Main attempt could not be verified for Codex assignment",
        )
    }

    fn require_lead_assignment_main(&self, attempts: &AttemptCache) -> Result<()> {
        require(
            self.caller == self.main.id
                && std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_none(),
            "native provider scheduling requires the current Main",
        )?;
        let repo = self.repo.canonicalize().map_err(Error::new)?;
        let main = attempts.resolve_attempt(&repo, &self.attempt, self.services, self.stop)?;
        require(
            main.is_some_and(|main| {
                self.is_live_main(&main) && main["refs"]["panel_native_scheduling"] == "unmanaged"
            }),
            "live Main attempt could not be verified for lead assignment",
        )?;
        self.require_exact_main_membership(&repo)
    }

    fn require_exact_main_membership(&self, repo: &Path) -> Result<()> {
        let canonical = repo
            .to_str()
            .ok_or_else(|| Error::new("repository cannot be represented in a Room locator"))?;
        let room = commands::query(
            self.services,
            NativeCommand::Room,
            repo,
            &[
                "show", "--repo", canonical, "--id", self.room, "--exact", "--json",
            ],
            self.stop,
        )?;
        require(
            self.has_live_main_membership(&room),
            "Main membership in this room could not be verified",
        )
    }

    fn assign_codex_alternate(
        &self,
        caller: &AlternateCaller,
        alias: &str,
        task: &str,
        host: &dyn AccountHost,
        tx: &Transaction<'_>,
    ) -> Result<Value> {
        validate_task(task)?;
        let main = self.alternate_main(caller, host)?;
        let repo = self.repo.canonicalize().map_err(Error::new)?;
        let attempts = AttemptCache::default();
        attempts.attempts(&repo, self.services, self.stop)?;
        // Refuse a changed ledger, including an unrelated append, during this explicit send.
        let stable_main = || {
            let current = attempts.current_attempt(&repo, &self.attempt, self.stop);
            self.require_live_codex_assignment_main(current.as_ref())?;
            tx.check()
        };
        stable_main()?;
        let primary = self.alternate_primary(&main, host)?;
        let selected = self.prove_alternate_binding(&main, &primary, host)?;
        require(
            selected.receipt.account_id.as_deref() == Some(alias),
            "requested account does not match the pinned Codex alternate lead",
        )?;

        let pane = &selected.pane;
        static TURN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let seed = format!(
            "{}:{}:{}:{}:{}:{}",
            self.attempt,
            pane.id,
            pane.birth,
            std::process::id(),
            TURN_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            oms_panel_core::now(),
        );
        let turn = oms_panel_core::digest(seed.as_bytes());
        let turn = &turn[..20];
        let locator = assignment_locator(&repo, self.room)?;
        let prompt = format!(
            "Task from Overall Main. Work within this task and report the result here. Turn {turn}. {ALTERNATE_STARTUP_PROMPT}{locator} Task: {task}"
        );
        // A single large Codex paste is replaced by an opaque placeholder in the composer.
        require(
            prompt.chars().count() <= CODEX_VISIBLE_PASTE_CHARS,
            "Codex alternate assignment is too long to verify in the composer",
        )?;

        let mut tmux = |args: &[&str]| self.tmux(args);
        let signature = alternate_pane_signature(&self.session, pane);
        let target = format!("{}:{}.{}", self.session, pane.window, pane.id);
        let mut fence = || {
            let fresh = self.reload()?;
            fresh.verify_current_alternate_main(self, &main, caller, host)?;
            let current_primary = fresh.alternate_primary(&main, host)?;
            let current = fresh.prove_alternate_binding(&main, &current_primary, host)?;
            require(
                primary.same_custody(&current_primary) && selected.same_custody(&current),
                "the Codex lead bindings changed during assignment",
            )?;
            stable_main()
        };
        fence()?;
        self.require_exact_main_membership(&repo)?;
        let first = CodexComposer::capture(&mut tmux, &target, &signature)?;
        require(first.ready(), ALTERNATE_INPUT_UNAVAILABLE)?;
        stable_main()?;
        // Check the exact Room after the last composer read at each mutation fence.
        let verify_main = || {
            self.require_exact_main_membership(&repo)?;
            stable_main()
        };
        let exact_turn = AlternateTurn {
            turn,
            signature: &signature,
            command: &first.command,
            verify_main: &verify_main,
        };
        paste_and_submit(
            "codex",
            &target,
            &prompt,
            Proof::AlternateCodexTurn(&exact_turn),
            &mut tmux,
            &mut fence,
            Pace {
                now: &mut Instant::now,
                sleep: &mut std::thread::sleep,
            },
        )?;
        Ok(alternate_result("submitted", selected.receipt.account_id))
    }

    #[cfg(test)]
    pub(crate) fn execute_with_host(
        &self,
        provider: &str,
        requested_account: Option<&str>,
        task: Option<&str>,
        host: &dyn AccountHost,
    ) -> Result<Value> {
        self.execute_with_host_cached(
            provider,
            requested_account,
            task,
            LeadLaunch {
                permissions: Some(MainPermissions::Full),
                ..Default::default()
            },
            LeadExecution {
                host,
                attempts: &AttemptCache::default(),
                transaction: None,
            },
        )
    }

    fn execute_with_host_cached(
        &self,
        provider: &str,
        requested_account: Option<&str>,
        task: Option<&str>,
        launch: LeadLaunch<'_>,
        execution: LeadExecution<'_>,
    ) -> Result<Value> {
        let LeadExecution {
            host,
            attempts,
            transaction,
        } = execution;
        let model = orchestrator_default_model(provider)?;
        if let Some(task) = task {
            require(
                !launch.explicit(),
                "orchestrator model and effort apply only when opening a new session",
            )?;
            validate_task(task)?;
        }
        if let Some(lead) = self.leads.get(provider) {
            require(
                !launch.explicit(),
                "provider session is already connected; model and effort cannot be changed by reconnecting",
            )?;
            let requested_receipt = if let Some(req_alias) = requested_account {
                let marked = self.tmux(&[
                    "show-options",
                    "-pqv",
                    "-t",
                    &lead.id,
                    REQUESTED_PROVIDER_MODEL,
                ])?;
                let model = if marked.is_empty() {
                    model
                } else {
                    require(
                        crate::cli::valid_model_id(&marked),
                        "live provider model marker is invalid",
                    )?;
                    &marked
                };
                let env_lines = self.tmux_env_lines()?;
                let env_refs: Vec<&str> = env_lines.iter().map(String::as_str).collect();
                Some(account_binding::resolve_selection(
                    host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider,
                        model,
                    },
                    Some(req_alias),
                    &env_refs,
                    oms_panel_core::timestamp(),
                )?)
            } else {
                None
            };
            let assignment = (provider == "codex" && task.is_some())
                .then(|| self.codex_assignment_custody(host))
                .transpose()?;
            let ordinary_binding = assignment
                .is_none()
                .then(|| self.live_binding(provider, lead, host))
                .flatten();
            let live_binding = assignment
                .as_ref()
                .and_then(|proof| proof.binding.as_ref())
                .or(ordinary_binding.as_ref());
            if let Some(req) = &requested_receipt {
                let Some(binding) = live_binding else {
                    return Err(Error::new(
                        "cannot rebind live provider session without a verified matching account binding",
                    ));
                };
                require(
                    binding.verification == AccountVerification::Verified
                        && binding.account_id == req.account_id
                        && binding.home_digest.is_some()
                        && binding.home_digest == req.home_digest,
                    "cannot rebind live provider session to a different account",
                )?;
            }
            let mut delivery = None;
            let status = if let Some(task) = task {
                static TURN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
                let stamp = format!(
                    "{:?}:{}:{}",
                    std::time::SystemTime::now(),
                    std::process::id(),
                    TURN_SEQUENCE.fetch_add(1, Ordering::Relaxed),
                );
                let turn = oms_panel_core::digest(
                    format!("{}:{provider}:{stamp}:{task}", self.attempt).as_bytes(),
                );
                let turn = &turn[..20];
                let locator = assignment
                    .as_ref()
                    .map(|proof| assignment_locator(&proof.repo, self.room))
                    .transpose()?
                    .unwrap_or_default();
                let role_contract = if provider == "codex" {
                    ALTERNATE_STARTUP_PROMPT
                } else {
                    STARTUP_PROMPT.trim()
                };
                let prompt = native_lead_prompt(turn, role_contract, &locator, task);
                if provider == "codex" {
                    self.send_checked(
                        provider,
                        &prompt,
                        Proof::Turn(turn),
                        assignment.as_ref().map(|proof| (proof, host, attempts)),
                        transaction.map(AssignmentEvidence::Primary),
                    )?;
                    "submitted"
                } else {
                    self.require_provider_input(provider)?;
                    let tx = transaction
                        .ok_or_else(|| Error::new("managed lead assignment receipt unavailable"))?;
                    let scope = lead_delivery::Scope::sample(self, provider, host)?;
                    let mut receipt = lead_delivery::Receipt::new(scope, turn);
                    // Retain an unverified receipt if a transport or writer is interrupted.
                    receipt.write(tx, provider)?;
                    crate::wakeup::notify_wakeup(self.repo);
                    let observed = self.send_checked(
                        provider,
                        &prompt,
                        Proof::LeadTurn {
                            turn,
                            agy_user_turn: &|| {
                                receipt.scope.proves_agy_turn(self.repo, turn, &prompt)
                            },
                        },
                        None,
                        Some(AssignmentEvidence::Lead(&receipt.scope, host, attempts)),
                    )?;
                    receipt.delivery = observed.delivery;
                    let recorded = receipt.write(tx, provider).is_ok();
                    if recorded {
                        crate::wakeup::notify_wakeup(self.repo);
                    }
                    delivery = Some((turn.to_owned(), observed, recorded));
                    if recorded && observed.delivery == lead_delivery::Delivery::TurnObserved {
                        "submitted"
                    } else {
                        "unknown"
                    }
                }
            } else {
                "connected"
            };
            let (account, account_pinned, account_verification) = binding_summary(live_binding);
            let mut output = json!({
                "status": status,
                "provider": provider,
                "account": account,
                "account_pinned": account_pinned,
                "account_verification": account_verification,
            });
            if let Some((turn, state, recorded)) = delivery {
                output["delivery"] = serde_json::json!(state.delivery);
                output["turn_id"] = turn.into();
                output["task_completion"] = "unconfirmed".into();
                output["receipt_recorded"] = recorded.into();
                if status == "unknown" {
                    output["reconcile_required"] = true.into();
                    output["reason"] = state.reconcile_cue().into();
                }
            }
            Ok(output)
        } else if task.is_some() {
            if let Some(req_alias) = requested_account {
                let env_lines = self.tmux_env_lines()?;
                let env_refs: Vec<&str> = env_lines.iter().map(String::as_str).collect();
                let _ = account_binding::resolve_selection(
                    host,
                    SelectionTarget {
                        role: "orchestrator",
                        provider,
                        model,
                    },
                    Some(req_alias),
                    &env_refs,
                    oms_panel_core::timestamp(),
                )?;
            }
            Err(Error::new(
                "open the provider session before sending a task",
            ))
        } else {
            self.start_with_account(
                provider,
                LeadAccount::Default(requested_account),
                launch,
                host,
            )
        }
    }

    fn start_with_account(
        &self,
        provider: &str,
        account: LeadAccount<'_>,
        launch: LeadLaunch<'_>,
        host: &dyn AccountHost,
    ) -> Result<Value> {
        let (_pane_id, receipt) = self.open_with_host(provider, account, launch, host)?;
        Ok(json!({
            "status": "started",
            "provider": provider,
            "account": receipt.account_id,
            "account_pinned": receipt.account_pinned,
            "account_verification": receipt.verification,
        }))
    }

    fn send(
        &self,
        provider: &str,
        prompt: &str,
        proof: Proof,
        assignment: Option<(&CodexAssignment, &dyn AccountHost, &AttemptCache)>,
    ) -> Result<()> {
        self.send_checked(provider, prompt, proof, assignment, None)
            .map(|_| ())
    }

    fn send_checked(
        &self,
        provider: &str,
        prompt: &str,
        proof: Proof,
        assignment: Option<(&CodexAssignment, &dyn AccountHost, &AttemptCache)>,
        evidence: Option<AssignmentEvidence<'_>>,
    ) -> Result<lead_delivery::Outcome> {
        if provider == "codex"
            && !matches!(evidence, Some(AssignmentEvidence::Bound(_)))
            && matches!(proof, Proof::Turn(_))
        {
            require(
                prompt.chars().count() <= CODEX_VISIBLE_PASTE_CHARS,
                "Codex assignment is too long to verify in the composer",
            )?;
        }
        self.require_provider_input(provider)?;
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("open the provider session before sending a task"))?;
        let fence = |before_typing: bool| {
            if let Some((expected, _, attempts)) = assignment {
                self.require_codex_assignment_main(&expected.repo, attempts)?;
            }
            let fresh = self.reload()?;
            require(
                self.main == fresh.main && fresh.leads.get(provider) == Some(lead),
                if before_typing {
                    "provider or Main pane changed before typing"
                } else {
                    "provider or Main pane changed"
                },
            )?;
            if let Some((expected, host, _)) = assignment {
                require(
                    self.session_proof == fresh.session_proof
                        && self.attempt == fresh.attempt
                        && self.owner == fresh.owner
                        && self.caller == fresh.main.id,
                    "Codex assignment Main or session changed",
                )?;
                let current = fresh.codex_assignment_custody(host)?;
                require(
                    expected.same_custody(&current),
                    "Codex assignment custody changed",
                )?;
            }
            match evidence {
                Some(AssignmentEvidence::Bound(binding)) => binding.revalidate(self)?,
                Some(AssignmentEvidence::Lead(scope, host, attempts)) => {
                    require(
                        self.owner == fresh.owner
                            && &lead_delivery::Scope::sample(&fresh, provider, host)? == scope,
                        "provider or Main process generation changed",
                    )?;
                    fresh.require_lead_assignment_main(attempts)?;
                }
                _ => {}
            }
            Ok(())
        };
        fence(true)?;
        let ticket = match (evidence, assignment, proof) {
            (Some(AssignmentEvidence::Primary(tx)), Some((custody, _, _)), Proof::Turn(turn)) => {
                native_assignment::PrimaryTicket::prepare(self, custody, tx, turn, prompt)?
            }
            _ => None,
        };
        let mut before_paste = true;
        paste_and_submit(
            provider,
            &lead.id,
            ticket.as_ref().map_or(prompt, |ticket| &ticket.prompt),
            proof,
            &mut |args| self.tmux(args),
            &mut || {
                fence(false)?;
                if let Some(ticket) = &ticket {
                    ticket.revalidate(self)?;
                    if std::mem::take(&mut before_paste) {
                        self.require_provider_input(provider)?;
                    }
                }
                Ok(())
            },
            Pace {
                now: &mut Instant::now,
                sleep: &mut std::thread::sleep,
            },
        )
    }

    fn require_provider_input(&self, provider: &str) -> Result<()> {
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("open the provider session before sending a task"))?;
        require(
            !lead.active,
            "provider window is focused; use it directly or return to Main",
        )?;
        let cursor = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &lead.id,
            "#{pane_in_mode}|#{cursor_x}|#{cursor_y}",
        ])?;
        let row = cursor
            .strip_prefix("0|2|")
            .and_then(|row| row.parse::<u32>().ok())
            .ok_or_else(|| Error::new("provider cursor is outside the start of its input"))?
            .to_string();
        let input = self.tmux(&["capture-pane", "-p", "-t", &lead.id, "-S", &row, "-E", &row])?;
        require(
            input.starts_with(PROMPT_MARKERS),
            "provider cursor does not point to a native prompt",
        )?;
        let capture = self.tmux(&["capture-pane", "-p", "-e", "-S", &row, "-t", &lead.id])?;
        require(
            input_ready(provider, &capture),
            "provider input is busy or a draft is present; try again when ready",
        )
    }
}

fn native_lead_prompt(turn: &str, role_contract: &str, locator: &str, task: &str) -> String {
    format!(
        "{} Turn {turn}. You own this provider's work. {role_contract}{locator} Task: {task}",
        crate::native_children::TASK_PREFIX,
    )
}

fn validate_task(task: &str) -> Result<()> {
    require(
        !task.trim().is_empty() && task.len() <= 4096 && !task.chars().any(char::is_control),
        "provider task must be a nonempty single line of up to 4096 bytes",
    )
}

const POLL_STEP: Duration = Duration::from_millis(100);
const PASTE_DEADLINE: Duration = Duration::from_secs(2);
const SUBMIT_DEADLINE: Duration = Duration::from_secs(5);
const PROMPT_MARKERS: [char; 3] = ['>', '❯', '›'];
const UNCONFIRMED: &str =
    "task submission is unconfirmed; check the provider pane and do not resend blindly";

const ALTERNATE_INPUT_UNAVAILABLE: &str =
    "the Codex alternate composer is unavailable, busy, selected, or holds a draft";

#[derive(PartialEq, Eq)]
pub(crate) struct CodexComposer {
    command: String,
    cursor_x: u32,
    cursor_y: usize,
    screen: String,
}

impl CodexComposer {
    pub(crate) fn for_main(frame: &str) -> Result<Self> {
        let unavailable = || Error::new("Main composer is unavailable, busy, or has a draft");
        let (identity, screen) = frame.split_once('\n').ok_or_else(unavailable)?;
        let fields: Vec<_> = identity.split('\t').collect();
        let ["0", "0", x, y, command, "."] = fields.as_slice() else {
            return Err(unavailable());
        };
        require(
            !command.is_empty() && command.len() <= 128 && !command.chars().any(char::is_control),
            "Main composer command is unverified",
        )?;
        Ok(Self {
            command: (*command).into(),
            cursor_x: x.parse().map_err(|_| unavailable())?,
            cursor_y: y.parse().map_err(|_| unavailable())?,
            screen: screen.into(),
        })
    }

    fn capture(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
        signature: &str,
    ) -> Result<Self> {
        let unavailable = || Error::new(ALTERNATE_INPUT_UNAVAILABLE);
        // The delimiter after the pane-local option prevents screen text from supplying a
        // missing option. The direct sample checks the exact pane and single-pane window.
        let frame = tmux(&[
            "display-message",
            "-p",
            "-t",
            pane,
            ALTERNATE_TASK_INPUT_FORMAT,
            ";",
            "show-options",
            "-pqv",
            "-t",
            pane,
            "@oms_provider_parent",
            ";",
            "display-message",
            "-p",
            "-t",
            pane,
            ".",
            ";",
            "capture-pane",
            "-p",
            "-e",
            "-J",
            "-S",
            "0",
            "-t",
            pane,
        ])?;
        let (identity, screen) = frame.split_once('\n').ok_or_else(unavailable)?;
        let fields: Vec<&str> = identity
            .strip_prefix(signature)
            .and_then(|tail| tail.strip_prefix('\t'))
            .ok_or_else(unavailable)?
            .split('\t')
            .collect();
        let ["0", "0", x, y, command, "."] = fields.as_slice() else {
            return Err(unavailable());
        };
        require(
            !command.is_empty() && command.len() <= 128 && !command.chars().any(char::is_control),
            ALTERNATE_INPUT_UNAVAILABLE,
        )?;
        let screen = screen
            .strip_prefix(ALTERNATE_LEAD_TAG)
            .and_then(|rest| rest.strip_prefix("\n.\n"))
            .ok_or_else(unavailable)?;
        Ok(Self {
            command: (*command).into(),
            cursor_x: x.parse().map_err(|_| unavailable())?,
            cursor_y: y.parse().map_err(|_| unavailable())?,
            screen: screen.into(),
        })
    }

    fn input(&self) -> Option<(&str, &str)> {
        let (area, _) = codex_chrome(&self.screen, CodexChromeUse::Composer)?;
        Self::input_in(area, self.cursor_y)
    }

    fn input_in(area: &str, cursor_y: usize) -> Option<(&str, &str)> {
        let mut lines = area.split_inclusive('\n');
        let mut offset = 0;
        let mut marker = None;
        for _ in 0..=cursor_y {
            let line = lines.next()?;
            let plain = CHAT_SGR.replace_all(line, "");
            if plain.starts_with("› ") || plain.trim_end_matches('\n') == "›" {
                marker = Some(offset);
            }
            offset += line.len();
        }
        let (above, _) = area.split_at(marker?);
        let input = area.get(above.len()..offset)?;
        // The cursor must be on the last input row. Any other visible text between it
        // and native chrome can be an attachment, popup, selection, or another draft row.
        (area.get(offset..)?.trim().is_empty()
            && !CHAT_SGR.find_iter(input).any(|sgr| {
                let escape = sgr.as_str();
                let mut params = escape[2..escape.len() - 1].split(';');
                while let Some(param) = params.next() {
                    match param {
                        "7" => return true,
                        "38" | "48" | "58" => match params.next() {
                            Some("5") => {
                                params.next();
                            }
                            Some("2") => {
                                params.nth(2);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                false
            }))
        .then_some((above, input))
    }

    fn empty(&self) -> bool {
        self.cursor_x == 2
            && self.input().is_some_and(|(_, input)| {
                let plain = CHAT_SGR.replace_all(input, "");
                input.lines().count() == 1
                    && matches!(plain.trim_end(), "›" | "› Ask Codex to do anything")
                    && input_ready("codex", input)
            })
    }

    pub(crate) fn ready(&self) -> bool {
        self.empty() && !self.busy()
    }

    fn busy(&self) -> bool {
        // Include the native prompt in the activity probe while avoiding long draft
        // continuations that would displace the near-prompt lines above it.
        self.input().is_some_and(|(above, input)| {
            let prompt = input.lines().next().unwrap_or_default();
            crate::provider_activity::observe("codex", &format!("{above}{prompt}")).is_some()
        })
    }

    pub(crate) fn exact_draft(&self, expected: &str) -> bool {
        if self.busy() {
            return false;
        }
        self.input()
            .is_some_and(|(_, input)| Self::draft_text_matches(input, expected, self.cursor_x))
    }

    fn draft_text_matches(input: &str, expected: &str, cursor_x: u32) -> bool {
        let plain = CHAT_SGR.replace_all(input, "");
        let Some(visible) = plain.strip_prefix("› ") else {
            return false;
        };
        let mut remaining = expected;
        let mut cursor_line = "";
        // Accept only visible text, without inventing separators at a soft wrap.
        // -J asks tmux to preserve spaces and join any terminal-generated wraps.
        for line in std::iter::once(Some(visible.lines().next().unwrap_or_default()))
            .chain(visible.lines().skip(1).map(|line| line.strip_prefix("  ")))
        {
            let Some(line) = line else { return false };
            let Some(rest) = remaining.strip_prefix(line) else {
                return false;
            };
            remaining = rest;
            cursor_line = line;
        }
        remaining.is_empty() && cursor_x as usize == crate::render::width(cursor_line) + 2
    }

    fn acknowledged(&self, turn: &str) -> bool {
        if !self.empty() {
            return false;
        }
        let Some((above, _)) = self.input() else {
            return false;
        };
        let plain = CHAT_SGR.replace_all(above, "");
        let rows: Vec<&str> = plain.lines().collect();
        let Some(user) = rows.iter().rposition(|row| row.starts_with("› ")) else {
            return false;
        };
        let mut words = rows[user..]
            .iter()
            .take_while(|row| !row.trim().is_empty())
            .flat_map(|row| row.split_whitespace());
        let id = format!("{turn}.");
        let mut previous = words.next();
        words.any(|word| {
            let matched = previous == Some("Turn") && word == id;
            previous = Some(word);
            matched
        })
    }
}

const ORDINARY_CODEX_DRAFT_FORMAT: &str = concat!(
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t",
    "#{pane_in_mode}\t#{pane_active}\t#{window_active}\t#{cursor_x}\t#{cursor_y}\t",
    "#{pane_width}\t#{pane_height}\t#{pane_current_command}\t.",
);

#[derive(PartialEq, Eq)]
enum OrdinaryCodexDraft {
    Exact(String),
    Opaque(String),
}

fn ordinary_codex_draft(
    tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
    pane: &str,
    expected: &str,
) -> Option<OrdinaryCodexDraft> {
    let frame = tmux(&[
        "display-message",
        "-p",
        "-t",
        pane,
        ORDINARY_CODEX_DRAFT_FORMAT,
        ";",
        "capture-pane",
        "-p",
        "-e",
        "-J",
        "-S",
        "0",
        "-t",
        pane,
    ])
    .ok()?;
    if frame.len() > MAX_CODEX_CAPTURE_BYTES || expected.is_empty() {
        return None;
    }
    let (identity, screen) = frame.split_once('\n')?;
    let fields: Vec<_> = identity.split('\t').collect();
    let [
        session,
        win,
        id,
        pid,
        "0",
        "0",
        active,
        focused,
        x,
        y,
        w,
        h,
        command,
        ".",
    ] = fields.as_slice()
    else {
        return None;
    };
    if *id != pane
        || !commands::valid_target(session, '$')
        || !commands::valid_target(win, '@')
        || !pid.parse::<u32>().is_ok_and(|pid| pid > 0)
        || !matches!((*active, *focused), ("0", "0" | "1") | ("1", "0"))
        || command.is_empty()
        || command.len() > 128
        || command.chars().any(char::is_control)
    {
        return None;
    }
    let (x, y, w, h) = (
        x.parse().ok()?,
        y.parse().ok()?,
        w.parse().ok()?,
        h.parse().ok()?,
    );
    if x >= w || y >= h {
        return None;
    }
    let area = codex_chrome(screen, CodexChromeUse::Composer)
        .map(|(area, _)| area)
        .or_else(|| ordinary_status_only_area(screen, y, h))?;
    let (above, input) = CodexComposer::input_in(area, y)?;
    let plain = CHAT_SGR.replace_all(input, "");
    if plain.lines().any(|line| {
        line.trim().is_empty()
            || line.chars().any(char::is_control)
            || crate::render::width(line) > w as usize
    }) {
        return None;
    }
    let prompt = input.lines().next()?;
    if oms_panel_core::tick::screen_input_empty(input).0
        || crate::provider_activity::observe("codex", &format!("{above}{prompt}")).is_some()
    {
        return None;
    }
    if codex_opaque_paste_placeholder(input) {
        return Some(OrdinaryCodexDraft::Opaque(frame));
    }
    CodexComposer::draft_text_matches(input, expected, x)
        .then_some(OrdinaryCodexDraft::Exact(frame))
}

fn ordinary_status_only_area(screen: &str, cursor_y: usize, height: usize) -> Option<&str> {
    // In this composer shape the last draft row, one blank row and one
    // indented native status occupy the bottom three physical rows.
    if cursor_y.checked_add(3)? != height || screen.lines().count() != height {
        return None;
    }
    let (before_status, status) = screen.rsplit_once('\n')?;
    let (input, blank) = before_status.rsplit_once('\n')?;
    let status = CHAT_SGR.replace_all(status, "");
    let blank = CHAT_SGR.replace_all(blank, "");
    if status.len() > MAX_CODEX_CHROME_ROW_BYTES
        || status.chars().any(char::is_control)
        || !status.starts_with("  ")
        || codex_footer_model(&status).is_none()
        || !blank.bytes().all(|b| b == b' ')
    {
        return None;
    }
    Some(input)
}

struct AlternateTurn<'a> {
    turn: &'a str,
    signature: &'a str,
    command: &'a str,
    verify_main: &'a dyn Fn() -> Result<()>,
}

impl AlternateTurn<'_> {
    fn capture(
        &self,
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
    ) -> Result<CodexComposer> {
        let capture = CodexComposer::capture(tmux, pane, self.signature)?;
        require(
            capture.command == self.command,
            "the Codex alternate pane command changed",
        )?;
        Ok(capture)
    }
}

/// What proves that a submitted prompt reached the provider.
#[derive(Clone, Copy)]
enum Proof<'a> {
    /// An ordinary Codex task carrying `Turn <turn_id>`.
    Turn(&'a str),
    /// Ordinary AGY or Claude lead. Only a provider-typed user record can
    /// turn a cleared composer into a confirmed delivery.
    LeadTurn {
        turn: &'a str,
        agy_user_turn: &'a dyn Fn() -> bool,
    },
    /// The explicitly pinned alternate requires an exact draft and whole-token terminal receipt.
    AlternateCodexTurn(&'a AlternateTurn<'a>),
    /// A command whose caller verifies its own effect, so Enter is the last step.
    Dispatched,
}

/// Injected time, so deadlines bound elapsed time rather than poll counts.
struct Pace<'a> {
    now: &'a mut dyn FnMut() -> Instant,
    sleep: &'a mut dyn FnMut(Duration),
}

impl Pace<'_> {
    /// Checks at least once, then until `check` holds or `deadline` has elapsed.
    fn poll(&mut self, deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
        let start = (self.now)();
        loop {
            if check() {
                return true;
            }
            if (self.now)().saturating_duration_since(start) >= deadline {
                return false;
            }
            (self.sleep)(POLL_STEP);
        }
    }
}

/// Pastes the task as one bracketed paste, presses Enter once only after the
/// draft is visible, then waits for the proof. Paste-burst detection can
/// swallow a keystroke Enter, so no path ever retypes or re-sends it.
fn paste_and_submit(
    provider: &str,
    pane: &str,
    prompt: &str,
    proof: Proof,
    tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
    fence: &mut dyn FnMut() -> Result<()>,
    mut pace: Pace,
) -> Result<lead_delivery::Outcome> {
    use lead_delivery::{Delivery, Outcome};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let uncertain = |message: String| Error {
        publication_uncertain: true,
        ..Error::new(message)
    };
    let seed = format!(
        "{pane}:{}:{}:{}:{prompt}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        oms_panel_core::now()
    );
    let buffer = format!(
        "oms-task-{}",
        &oms_panel_core::digest(seed.as_bytes())[..16]
    );
    tmux(&["set-buffer", "-b", &buffer, "--", &literal_tmux_arg(prompt)])?;
    let ordinary_lead = matches!(proof, Proof::LeadTurn { .. });
    let agy_preexisting_turn = if let Proof::LeadTurn { turn, .. } = proof
        && provider == "antigravity"
    {
        lead_delivery::Frame::capture(tmux, pane).is_ok_and(|frame| frame.shows_turn(turn))
    } else {
        false
    };
    let mut before_paste = || -> Result<()> {
        fence()?;
        if let Proof::AlternateCodexTurn(expected) = proof {
            require(
                expected.capture(tmux, pane)?.ready(),
                ALTERNATE_INPUT_UNAVAILABLE,
            )?;
            (expected.verify_main)()?;
        }
        Ok(())
    };
    if let Err(error) = before_paste() {
        let _ = tmux(&["delete-buffer", "-b", &buffer]);
        return Err(Error::new(format!("pane changed before pasting: {error}")));
    }
    if let Err(error) = tmux(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", pane]) {
        let _ = tmux(&["delete-buffer", "-b", &buffer]);
        if ordinary_lead {
            return Ok(Outcome::uncertain(
                "Paste outcome is unknown. Enter was withheld. Inspect the provider pane before resending.",
            ));
        }
        return Err(error);
    }
    let mut lead_draft = None;
    if let Proof::AlternateCodexTurn(expected) = proof {
        let mut previous = None;
        let settled = pace.poll(PASTE_DEADLINE, || {
            let draft = expected
                .capture(tmux, pane)
                .ok()
                .filter(|c| c.exact_draft(prompt));
            let settled = draft.is_some() && draft == previous;
            previous = draft;
            settled
        });
        let mut verified = || {
            require(
                settled,
                "the complete intended Codex draft could not be verified",
            )?;
            fence()?;
            require(
                Some(expected.capture(tmux, pane)?) == previous,
                "the Codex alternate capture changed",
            )?;
            (expected.verify_main)()
        };
        verified().map_err(|error| {
            uncertain(format!(
                "Enter was withheld after pasting. The draft may remain unsubmitted: {error}"
            ))
        })?;
    } else if provider == "codex" && matches!(proof, Proof::Turn(_)) {
        let mut previous = None;
        let settled = pace.poll(PASTE_DEADLINE, || {
            let draft = ordinary_codex_draft(tmux, pane, prompt);
            let settled = draft.is_some() && draft == previous;
            previous = draft;
            settled
        });
        if !settled {
            return Err(uncertain(
                "task paste was not observed; Enter was not sent and a draft may remain — inspect the provider input before resending".into(),
            ));
        }
        if matches!(previous, Some(OrdinaryCodexDraft::Opaque(_))) {
            return Err(uncertain(
                "Enter was withheld after pasting; Codex collapsed the draft into an opaque placeholder — inspect the provider input before resending".into(),
            ));
        }
        fence().map_err(|error| {
            uncertain(format!(
                "pane changed after pasting; the task draft may remain unsubmitted: {error}"
            ))
        })?;
        if ordinary_codex_draft(tmux, pane, prompt) != previous {
            return Err(uncertain(
                "Enter was withheld after pasting; the exact Codex draft changed before submission"
                    .into(),
            ));
        }
    } else if ordinary_lead {
        let mut previous = None;
        let settled = pace.poll(PASTE_DEADLINE, || {
            let draft = lead_delivery::Frame::exact_draft(tmux, pane, provider, prompt);
            let settled = draft.is_some() && draft == previous;
            previous = draft;
            settled
        });
        if !settled {
            return Ok(Outcome::uncertain(
                "The complete pasted draft could not be verified. Enter was withheld. Inspect the provider pane before resending.",
            ));
        }
        if lead_delivery::Frame::exact_draft(tmux, pane, provider, prompt) != previous {
            return Ok(Outcome::uncertain(
                "The pasted draft changed. Enter was withheld. Inspect the provider pane before resending.",
            ));
        }
        if fence().is_err() {
            return Ok(Outcome::uncertain(
                "Room, Main, or provider custody changed after pasting. Enter was withheld. Inspect the provider pane before resending.",
            ));
        }
        lead_draft = previous;
    } else {
        let mut previous = None;
        let settled = pace.poll(PASTE_DEADLINE, || {
            let draft = prompt_capture(tmux, pane, false)
                .map(|(_, capture)| capture)
                .filter(|c| {
                    if provider == "codex" {
                        codex_input_state(c) == Some(false)
                    } else {
                        !input_ready(provider, c)
                    }
                });
            let settled = draft.is_some() && draft == previous;
            previous = draft;
            settled
        });
        if !settled {
            return Err(uncertain(
                "task paste was not observed; Enter was not sent and a draft may remain — inspect the provider input before resending".into(),
            ));
        }
        fence().map_err(|error| {
            uncertain(format!(
                "pane changed after pasting; the task draft may remain unsubmitted: {error}"
            ))
        })?;
    }
    if tmux(&["send-keys", "-t", pane, "Enter"]).is_err() {
        return if ordinary_lead {
            Ok(Outcome::uncertain(
                "Enter may have reached the provider. Inspect the provider pane before resending.",
            ))
        } else {
            Err(uncertain(UNCONFIRMED.into()))
        };
    }
    let confirmed = match proof {
        Proof::Dispatched => return Ok(Delivery::Unverified.into()),
        Proof::LeadTurn { agy_user_turn, .. } => {
            let scope_lost = || {
                Outcome::uncertain(
                    "Room, Main, or provider custody changed after Enter. Inspect the provider pane before resending.",
                )
            };
            let mut delivery = Delivery::Unverified;
            pace.poll(SUBMIT_DEADLINE, || {
                let Ok(frame) = lead_delivery::Frame::capture(tmux, pane) else {
                    delivery = Delivery::Unverified;
                    return false;
                };
                delivery = frame.delivery(provider, lead_draft.as_ref());
                delivery == Delivery::InputClearedUnverified
            });
            if fence().is_err() {
                return Ok(scope_lost());
            }
            if provider == "antigravity"
                && !agy_preexisting_turn
                && delivery == Delivery::InputClearedUnverified
            {
                // The exact typed request can outlive a wrapped or scrolled-off user row.
                let confirmed = agy_user_turn();
                if fence().is_err() {
                    return Ok(scope_lost());
                }
                if confirmed {
                    delivery = Delivery::TurnObserved;
                }
            }
            return Ok(delivery.into());
        }
        Proof::Turn(turn_id) => {
            let turn = format!("Turn {turn_id}");
            pace.poll(SUBMIT_DEADLINE, || codex_submitted(pane, &turn, tmux))
        }
        Proof::AlternateCodexTurn(expected) => pace.poll(SUBMIT_DEADLINE, || {
            expected
                .capture(tmux, pane)
                .is_ok_and(|c| c.acknowledged(expected.turn))
        }),
    };
    require(confirmed, UNCONFIRMED).map_err(|_| uncertain(UNCONFIRMED.into()))?;
    Ok(Delivery::TurnObserved.into())
}

/// An empty input alone may be a cleared draft, so Codex must also show the
/// turn in a submitted user message. One capture spans the history through the
/// editable prompt row, keeping the cut point consistent if the screen scrolls.
fn codex_submitted(
    pane: &str,
    turn: &str,
    tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
) -> bool {
    let Some((row, input)) = prompt_capture(tmux, pane, true) else {
        return false;
    };
    input_ready("codex", &input)
        && tmux(&[
            "capture-pane",
            "-p",
            "-J",
            "-S",
            "-200",
            "-E",
            &format!("{row}"),
            "-t",
            pane,
        ])
        .is_ok_and(|capture| codex_turn_shown(&capture, turn))
}

/// Whether a Codex user message (a `›` row plus its continuation rows up to a
/// blank row) above the editable prompt contains `turn`. The capture's final
/// row must be that prompt and is excluded; any other final row proves nothing.
fn codex_turn_shown(capture: &str, turn: &str) -> bool {
    let plain = CHAT_SGR.replace_all(capture, "");
    let mut rows: Vec<&str> = plain.lines().collect();
    if !rows.pop().is_some_and(|row| row.starts_with('›')) {
        return false;
    }
    let Some(turn_id) = turn.strip_prefix("Turn ") else {
        return false;
    };
    let Some(user) = rows.iter().rposition(|row| row.starts_with("› ")) else {
        return false;
    };
    let expected = format!("{turn_id}.");
    let mut words = rows[user..]
        .iter()
        .take_while(|row| !row.trim().is_empty())
        .flat_map(|row| row.split_whitespace());
    let mut previous = words.next();
    words.any(|word| {
        let matched = previous == Some("Turn") && word == expected;
        previous = Some(word);
        matched
    })
}

/// Captures the input from its native prompt row with that row, or None while
/// it is not observable. An empty input keeps the cursor just after the marker;
/// a draft moves it right or, once wrapped, below the marker row.
fn prompt_capture(
    tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
    pane: &str,
    empty: bool,
) -> Option<(usize, String)> {
    let cursor = tmux(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_in_mode}|#{cursor_x}|#{cursor_y}",
    ])
    .ok()?;
    let (x, y) = cursor.strip_prefix("0|")?.split_once('|')?;
    let y = y.parse::<usize>().ok()?;
    let row = if empty {
        (x == "2").then_some(y)?
    } else {
        // Trimmed output may drop blank rows, so the marker check below rejects a miscount.
        let rows = format!("{y}");
        let screen = tmux(&["capture-pane", "-p", "-t", pane, "-S", "0", "-E", &rows]).ok()?;
        let back = screen
            .lines()
            .rev()
            .position(|line| line.starts_with(PROMPT_MARKERS))?;
        y.checked_sub(back)?
    };
    let capture = tmux(&[
        "capture-pane",
        "-p",
        "-e",
        "-S",
        &format!("{row}"),
        "-t",
        pane,
    ])
    .ok()?;
    let first = capture.lines().next()?;
    CHAT_SGR
        .replace_all(first, "")
        .starts_with(PROMPT_MARKERS)
        .then_some((row, capture))
}

fn literal_tmux_arg(text: &str) -> Cow<'_, str> {
    // tmux consumes trailing semicolons as command separators even with send-keys -l.
    match text.strip_suffix(';') {
        Some(prefix) => Cow::Owned(format!(r"{prefix}\;")),
        None => Cow::Borrowed(text),
    }
}

fn project_bound_codex_quota(
    report: &mut Value,
    alias: &str,
    now: f64,
    weekly_proven: bool,
    ambient_proven: bool,
) {
    use oms_panel_core::metrics::{READING_FRESH_SECONDS, number, percent, stamp};

    if let Some(fields) = report.as_object_mut() {
        fields.remove("codex_weekly_account");
    }
    let Some(status) = report
        .get_mut("provider_status")
        .and_then(|providers| providers.get_mut("codex"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    // Only the per-home session log projection supplies the bound weekly reading.
    let weekly = status.get("accounts").and_then(|accounts| {
        if !weekly_proven || !crate::cli::valid_account_id(alias) || !now.is_finite() || now < 0.0 {
            return None;
        }
        let row = accounts.get(alias)?;
        let used = percent(&row["weekly_used"])?;
        let observed = stamp(&row["observed_at"])?;
        let reset = stamp(&row["resets_at"])?;
        (row["source"] == "session log"
            && observed.is_finite()
            && (0.0..=now).contains(&observed)
            && reset.is_finite()
            && reset > observed)
            .then_some((used, observed, reset))
    });
    let matching_label =
        ambient_proven && status.get("account_label").and_then(Value::as_str) == Some(alias);
    if !matching_label {
        status.remove("account_label");
    }
    let bound_ambient =
        matching_label && status.get("source").and_then(Value::as_str) == Some("session log");
    let ambient_at = status.get("observed_at").and_then(stamp);
    let fresh_short = |used: &Value, reset: &Value, observed: Option<f64>| {
        let used = percent(used)?;
        let reset = stamp(reset)?;
        let at = observed?;
        (at <= now && now - at <= READING_FRESH_SECONDS && reset > now).then_some((used, reset, at))
    };
    // The per-home parser does not export short windows. Their ambient evidence
    // is useful only when its account label matches the live binding.
    let has_short_windows = status.contains_key("short_windows");
    let mut short_at: Option<f64> = None;
    let mut five_hour = None;
    if bound_ambient
        && let Some(windows) = status
            .get_mut("short_windows")
            .and_then(Value::as_array_mut)
    {
        windows.retain(|window| {
            let Some(minutes) =
                number(&window["minutes"]).filter(|&n| n > 0.0 && n != 10080.0 && n.fract() == 0.0)
            else {
                return false;
            };
            let Some(sample @ (_, _, at)) = fresh_short(
                &window["used"],
                &window["resets_at"],
                stamp(&window["observed_at"]),
            ) else {
                return false;
            };
            if minutes == 300.0 && five_hour.is_none() {
                five_hour = Some(sample);
            }
            short_at = Some(short_at.map_or(at, |previous| previous.max(at)));
            true
        });
    }
    if short_at.is_none() {
        status.remove("short_windows");
    }
    if bound_ambient && !has_short_windows {
        five_hour = status
            .get("five_hour_used")
            .and_then(|used| fresh_short(used, status.get("five_hour_resets_at")?, ambient_at));
    }
    for key in [
        "weekly_used",
        "weekly_resets_at",
        "weekly_native_observed_at",
        "observed_at",
        "five_hour_used",
        "five_hour_resets_at",
    ] {
        status.remove(key);
    }
    if let Some((used, reset, _)) = five_hour {
        status.insert("five_hour_used".into(), json!(used));
        status.insert("five_hour_resets_at".into(), json!(reset));
    }
    if let Some((used, observed, reset)) = weekly {
        status.insert("weekly_used".into(), json!(used));
        status.insert("weekly_resets_at".into(), json!(reset));
        status.insert("observed_at".into(), json!(observed));
        status.insert("source".into(), json!("session log"));
    } else if let Some(observed) = short_at.or(five_hour.map(|(_, _, at)| at)) {
        status.insert("observed_at".into(), json!(observed));
    } else {
        status.insert("source".into(), json!("unavailable"));
    }
    if weekly.is_some() {
        report["codex_weekly_account"] = json!(alias);
    }
}

fn weekly_from_usage(provider: &str, screen: &str) -> Option<f64> {
    if provider == "antigravity" {
        return agy_weekly_from_dialog(screen);
    }
    if provider != "claude"
        || !screen.lines().any(|line| {
            line.contains("Settings") && line.contains("Status") && line.contains("Usage")
        })
    {
        return None;
    }
    let mut lines = screen.lines().map(str::trim);
    lines
        .by_ref()
        .take_while(|line| *line != "CLAUDE AND GPT MODELS")
        .find(|line| *line == "Current week (all models)")?;
    // Locate the amount on the next rendered row inside the selected weekly window.
    let progress = lines.next()?;
    let (pct, suffix) = progress.split_once("% used")?;
    if !suffix.trim().is_empty() {
        return None;
    }
    let pct = pct.split_whitespace().next_back()?.parse::<f64>().ok()?;
    (0.0..=100.0).contains(&pct).then_some(pct)
}

fn agy_weekly_from_dialog(screen: &str) -> Option<f64> {
    if screen.len() > 64 * 1024 {
        return None;
    }
    let mut dialog = screen.lines().filter(|line| !line.trim().is_empty());
    dialog.find(|line| line.trim() == "└ Models & Quota")?;
    let first = dialog.next()?;
    let headed = first.starts_with("  ")
        && first.trim().strip_prefix("Account:").is_some_and(|value| {
            value.is_empty()
                || (value.starts_with(' ')
                    && value.len() <= 128
                    && value.bytes().all(|b| b.is_ascii_graphic() || b == b' '))
        });
    let mut next = || dialog.next().map(str::trim);
    let group = if headed { next()? } else { first.trim() };
    if group != "GEMINI MODELS" || (headed && !agy_quota_models(next()?)) {
        return None;
    }
    // capture-pane -p preserves the spaces inside the native colored bars.
    let bar_glyph = if headed { ' ' } else { '█' };
    const WEEKLY: &str = "Weekly Limit Remaining";
    if next() != Some(WEEKLY) {
        return None;
    }
    let remaining = agy_quota_amount(next()?, bar_glyph)?;
    if (headed && next() != Some("Quota available")) || next() != Some("Five Hour Limit Remaining")
    {
        return None;
    }
    agy_quota_amount(next()?, bar_glyph)?;
    if (headed && next() != Some("Quota available"))
        || next() != Some("CLAUDE AND GPT MODELS")
        || (headed && !agy_quota_models(next()?))
        || next() != Some(WEEKLY)
    {
        return None;
    }
    agy_quota_amount(next()?, bar_glyph)?;
    if headed
        && (!agy_first_quota_page(next()?)
            || next()
                != Some(
                    "↑/↓ Scroll · pgup/pgdown Page · ctrl+end Bottom · ctrl+home Top · esc Close",
                ))
    {
        return None;
    }
    // An unrecognized continuation could be a newer prompt or another dialog.
    if let Some(line) = dialog.next()
        && (!headed || !agy_quota_status_line(line))
    {
        return None;
    }
    dialog.next().is_none().then_some(100.0 - remaining)
}

fn agy_first_quota_page(line: &str) -> bool {
    let range = line
        .strip_prefix("(1–")
        .and_then(|line| line.strip_suffix(" lines)"))
        .and_then(|line| line.split_once(" of "));
    let count = |text: &str| {
        (!text.is_empty() && text.len() <= 3 && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u16>().ok())
            .flatten()
    };
    range.is_some_and(|(end, total)| {
        matches!((count(end), count(total)), (Some(end), Some(total)) if end > 1 && end < total)
    })
}

fn agy_quota_models(line: &str) -> bool {
    line.strip_prefix("Models within this group: ")
        .is_some_and(|list| list.len() <= 256 && list.split(", ").all(agy_quota_model_label))
}

fn agy_quota_model_label(model: &str) -> bool {
    model.len() <= 80
        && model
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " .-()_/".contains(c))
}

fn agy_quota_status_line(line: &str) -> bool {
    line.starts_with("  ")
        && line
            .trim()
            .split_once(" · ")
            .is_some_and(|(model, effort)| {
                agy_quota_model_label(model)
                    && matches!(effort, "low" | "medium" | "high" | "xhigh" | "max")
            })
}

fn agy_quota_amount(line: &str, bar_glyph: char) -> Option<f64> {
    let (bar, amount) = line.strip_prefix('[')?.split_once("] ")?;
    if bar.is_empty() || !bar.chars().all(|glyph| glyph == bar_glyph) {
        return None;
    }
    let digits = amount.trim_start().strip_suffix('%')?;
    let (whole, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(whole, fraction)| (whole, Some(fraction)));
    if !(1..=3).contains(&whole.len())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|part| {
            !(1..=2).contains(&part.len()) || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    let value = digits.parse::<f64>().ok()?;
    (0.0..=100.0).contains(&value).then_some(value)
}

/// One weekly provenance per round: the newest of the dialog sample and the
/// lead HUD reading, and only when newer than the row's `observed_at`. Dialog
/// wins ties. A stale lead reading marks the row stale and drops its number.
fn merge_weekly(
    status: &mut Value,
    dialog: Option<(f64, f64)>,
    lead: Option<&crate::claude_lead_usage::LeadReading>,
) {
    let floor = oms_panel_core::metrics::stamp(&status["observed_at"]);
    let newer = |at: f64| floor.is_none_or(|old| old < at);
    let dialog = dialog.filter(|(_, at)| newer(*at));
    let lead =
        lead.filter(|r| newer(r.observed_at) && dialog.is_none_or(|(_, at)| at < r.observed_at));
    let drop = |status: &mut Value, keys: &[&str]| {
        if let Some(row) = status.as_object_mut() {
            for key in keys {
                row.remove(*key);
            }
        }
    };
    match (lead, dialog) {
        (Some(reading), _) => match reading.weekly_used {
            Some(used) if !reading.stale => {
                drop(status, &["weekly_native_observed_at", "weekly_resets_at"]);
                status["weekly_used"] = json!(used);
                status["weekly_lead_observed_at"] = json!(reading.observed_at);
                if let Some(reset) = reading.resets_at {
                    status["weekly_resets_at"] = json!(reset);
                }
            }
            _ => {
                drop(
                    status,
                    &[
                        "weekly_used",
                        "weekly_resets_at",
                        "weekly_native_observed_at",
                        "weekly_lead_observed_at",
                    ],
                );
                status["source"] = json!("cache stale");
                status["observed_at"] = json!(reading.observed_at);
            }
        },
        (None, Some((used, observed_at))) => {
            drop(status, &["weekly_lead_observed_at"]);
            status["weekly_used"] = json!(used);
            status["weekly_native_observed_at"] = json!(observed_at);
        }
        (None, None) => {}
    }
}

fn project_pinned_claude_quota(
    status: &mut Value,
    proved: Option<(&str, LeadReading)>,
    lead: bool,
) {
    if !status.is_object() && proved.is_some() {
        *status = json!({});
    }
    let Some(fields) = status.as_object_mut() else {
        return;
    };
    for key in [
        "account_label",
        "observed_at",
        "weekly_used",
        "weekly_resets_at",
        "weekly_lead_observed_at",
        "weekly_native_observed_at",
        "five_hour_used",
        "five_hour_resets_at",
        "short_windows",
        "resets_at",
    ] {
        fields.remove(key);
    }
    fields.insert("source".into(), json!("unavailable"));
    if let Some((alias, reading)) = proved {
        // The previous row was ambient. Its timestamp cannot rank this reading.
        merge_weekly(status, None, Some(&reading));
        if !lead && let Some(fields) = status.as_object_mut() {
            fields.remove("weekly_lead_observed_at");
        }
        if reading.weekly_used.is_some() && !reading.stale {
            status["account_label"] = json!(alias);
            status["source"] = json!("HUD cache");
            status["observed_at"] = json!(reading.observed_at);
        }
    }
}

fn valid_attempt(attempt: &str) -> bool {
    attempt.strip_prefix("att_").is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn live_provider(fields: &[&str], session: &str, main_pane: &str) -> Option<&'static str> {
    if fields.len() != 15 || fields[0] != session || fields[4] != "0" || fields[2] == main_pane {
        return None;
    }
    PROVIDERS
        .into_iter()
        .find(|provider| *provider == fields[11])
}

fn alternate_pane_signature(session: &str, pane: &PaneIdentity) -> String {
    format!(
        "{session}\t{}\t{}\t{}\t0\t1\t.",
        pane.window, pane.id, pane.pid
    )
}

fn alternate_result(status: &str, account: Option<String>) -> Value {
    json!({
        "status": status,
        "provider": "codex",
        "lead": "alternate",
        "account": account,
        "account_pinned": true,
        "account_verification": "verified",
    })
}

fn pane(f: &[&str]) -> Result<Pane> {
    require(
        commands::valid_target(f[1], '@') && commands::valid_target(f[2], '%'),
        "invalid pane binding",
    )?;
    Ok(Pane {
        window: f[1].into(),
        id: f[2].into(),
        pid: f[3].parse().map_err(Error::new)?,
        active: f[5] == "1" && f[13] == "1",
    })
}

fn inherited_authority(key: &str) -> bool {
    key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && ([
            "OMS_",
            "CODEX_THREAD_",
            "CODEX_SESSION_",
            "CLAUDE_CODE_",
            "AGY_SESSION_",
            "ANTIGRAVITY_SESSION_",
        ]
        .iter()
        .any(|prefix| key.starts_with(prefix))
            || [
                "CLAUDECODE",
                "CLAUDE_SESSION_ID",
                "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
            ]
            .contains(&key))
}

fn input_ready(provider: &str, capture: &str) -> bool {
    if provider == "codex" {
        return codex_input_state(capture) == Some(true);
    }
    // AGY 1.5 represents this placeholder with a dedicated foreground instead of dim SGR.
    const AGY_HINT: &str = "Auto mode: edits & safe commands auto-approved (shift+tab to cycle)";
    if provider == "antigravity" {
        let hint = format!("\x1b[38;5;109m{AGY_HINT}\x1b[39m");
        let dim = format!("\x1b[2m{AGY_HINT}\x1b[0m");
        return oms_panel_core::tick::screen_input_empty(&capture.replace(&hint, &dim)).0;
    }
    oms_panel_core::tick::screen_input_empty(capture).0
}

const MAX_CODEX_CAPTURE_BYTES: usize = 1_048_576;
const MAX_CODEX_CHROME_ROW_BYTES: usize = 4096;
const CODEX_KEY_HELP: &str = "← for agents · ? for shortcuts";

fn codex_key_hint(line: &str) -> bool {
    line.contains("← for agents") || line.contains("? for shortcuts")
}

fn codex_prompt_marker(line: &str) -> bool {
    let mut characters = line.chars();
    characters
        .next()
        .is_some_and(|c| PROMPT_MARKERS.contains(&c))
        && characters
            .next()
            .is_none_or(|c| matches!(c, ' ' | '\u{a0}'))
}

fn codex_input_state(capture: &str) -> Option<bool> {
    if capture.len() > MAX_CODEX_CAPTURE_BYTES {
        return None;
    }
    let chrome = codex_chrome(capture, CodexChromeUse::Composer);
    let input = chrome.as_ref().map_or(capture, |(area, _)| area);
    let plain = CHAT_SGR.replace_all(input, "");
    let mut lines = plain.lines();
    let first = lines.next()?;
    if !codex_prompt_marker(first) || first.chars().any(char::is_control) {
        return None;
    }
    let mut below_rule = false;
    for line in lines
        .map(|line| line.trim_matches(' '))
        .filter(|line| !line.is_empty())
    {
        let trimmed = line.trim_start();
        let marker = trimmed
            .strip_prefix(['│', '┃', '|'])
            .map_or(trimmed, str::trim_start);
        // Match the generic recognizer's box-side marker grammar on later
        // rows, and never hide footer text behind the lower rule it stops at.
        if below_rule
            || codex_prompt_marker(marker)
            || line.chars().any(char::is_control)
            || (chrome.is_none() && (line.contains(" · Context") || codex_key_hint(line)))
        {
            return None;
        }
        // Mirror screen_input_empty's rule and side cells for this outer fence.
        const RULES: &str = "─━═╌┄╭╮╰╯┌┐└┘├┤┬┴┼";
        below_rule = line
            .chars()
            .all(|c| RULES.contains(c) || "│┃| ".contains(c))
            && line.chars().any(|c| RULES.contains(c));
    }
    Some(oms_panel_core::tick::screen_input_empty(input).0)
}

fn is_pasted_content_token(text: &str) -> bool {
    let Some(body) = text.strip_prefix("[Pasted Content ") else {
        return false;
    };
    let Some(digits) = body
        .strip_suffix(" chars]")
        .or_else(|| body.strip_suffix(" char]"))
    else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn codex_opaque_paste_placeholder(capture: &str) -> bool {
    if codex_input_state(capture) != Some(false) {
        return false;
    }
    let input = codex_chrome(capture, CodexChromeUse::Composer).map_or(capture, |(area, _)| area);
    let plain = CHAT_SGR.replace_all(input, "");
    let mut lines = plain.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let Some(rest) = first.strip_prefix(PROMPT_MARKERS) else {
        return false;
    };
    let trimmed = rest.trim_matches([' ', '\u{a0}']);
    let head = trimmed
        .strip_suffix(['│', '┃', '|'])
        .unwrap_or(trimmed)
        .trim_matches([' ', '\u{a0}']);
    if !head.starts_with('[') {
        return false;
    }
    let mut parts = vec![head];
    const RULES: &str = "─━═╌┄╭╮╰╯┌┐└┘├┤┬┴┼";
    for raw_line in lines {
        let line = raw_line.trim_matches(' ');
        if line.is_empty() {
            continue;
        }
        if line
            .chars()
            .all(|c| RULES.contains(c) || "│┃| ".contains(c))
            && line.chars().any(|c| RULES.contains(c))
        {
            break;
        }
        let cell = line
            .strip_prefix(['│', '┃', '|'])
            .unwrap_or(line)
            .trim_matches([' ', '\u{a0}']);
        let cell = cell
            .strip_suffix(['│', '┃', '|'])
            .unwrap_or(cell)
            .trim_matches([' ', '\u{a0}']);
        if !cell.is_empty() {
            if parts.len() == 4 {
                return false;
            }
            parts.push(cell);
        }
    }
    is_pasted_content_token(&parts.join(" ")) || is_pasted_content_token(&parts.concat())
}

fn codex_chrome_row<'a>(
    rows: &mut impl Iterator<Item = &'a str>,
) -> Option<(&'a str, Cow<'a, str>)> {
    for raw in rows {
        if raw.len() > MAX_CODEX_CHROME_ROW_BYTES {
            return None;
        }
        let plain = CHAT_SGR.replace_all(raw, "");
        if plain.chars().any(char::is_control) {
            return None;
        }
        if !plain.trim_matches(' ').is_empty() {
            return Some((raw, plain));
        }
    }
    None
}

enum CodexChromeUse {
    Composer,
    ModelObservation,
}

fn codex_chrome(capture: &str, usage: CodexChromeUse) -> Option<(&str, String)> {
    if capture.len() > MAX_CODEX_CAPTURE_BYTES {
        return None;
    }
    let mut rows = capture.lines().rev();
    let (_, keys) = codex_chrome_row(&mut rows)?;
    let (raw_status, status) = codex_chrome_row(&mut rows)?;
    let warning = keys.trim_matches(' ').strip_prefix(CODEX_KEY_HELP)?;
    static CODEX_WARNING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^⚠ [0-9]+ warnings? · f2 to view$").expect("static warning pattern")
    });
    static CODEX_GOAL_CHROME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^Pursuing goal \([0-9]{1,4}h [0-5]?[0-9]m\)(?: · ⚠ [0-9]+ warnings? · f2 to view)?$",
        )
        .expect("static Codex goal chrome")
    });
    static CODEX_COMPLETED_GOAL_CHROME: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| {
            regex::Regex::new(concat!(
                r"^Goal achieved \((?:(?:[0-9]|[1-5][0-9])s|(?:[1-9]|[1-5][0-9])m|",
                r"(?:[1-9]|1[0-9]|2[0-3])h(?: (?:[1-9]|[1-5][0-9])m)?|",
                r"[1-9][0-9]{0,3}d (?:[0-9]|1[0-9]|2[0-3])h (?:[0-9]|[1-5][0-9])m",
                r")\)(?: · ⚠ [0-9]+ warnings? · f2 to view)?$",
            ))
            .expect("static Codex completed goal chrome")
        });
    if !warning.is_empty()
        && !warning.strip_prefix(' ').is_some_and(|text| {
            let tail = text.trim_start_matches(' ');
            CODEX_WARNING.is_match(tail)
                || CODEX_GOAL_CHROME.is_match(tail)
                // Completed goal text is evidence for the model display only.
                || (matches!(usage, CodexChromeUse::ModelObservation)
                    && CODEX_COMPLETED_GOAL_CHROME.is_match(tail))
        })
    {
        return None;
    }
    let model = codex_footer_model(&status)?.to_owned();
    // Key help and the status are the final visible rows. The split leaves the
    // raw composer styling available for the separate input-ownership proof.
    capture
        .rfind(raw_status)
        .map(|start| (&capture[..start], model))
}

/// Observe the displayed Codex model from a bounded pane capture whose last
/// visible rows are recognized native status and key help. This yields only
/// the normalized model. The caller proves pane identity and any input safety
/// separately. A clipped or status-only capture provides no observation.
pub(crate) fn codex_model_from_chrome(capture: &str) -> Option<String> {
    codex_chrome(capture, CodexChromeUse::ModelObservation).map(|(_, model)| model)
}

fn codex_footer_model(line: &str) -> Option<&str> {
    fn remaining(field: &str) -> bool {
        field.strip_suffix("% left").is_some_and(|percent| {
            (1..=3).contains(&percent.len())
                && percent.bytes().all(|byte| byte.is_ascii_digit())
                && percent.parse::<u8>().is_ok_and(|value| value <= 100)
        })
    }

    let (header, context) = line.trim().split_once(" · Context ")?;
    let mut fields = context.split(" · ");
    if !remaining(fields.next()?) {
        return None;
    }
    let mut branch = fields.next();
    if let Some(weekly) = branch.and_then(|field| field.strip_prefix("weekly ")) {
        if !remaining(weekly) {
            return None;
        }
        branch = fields.next();
    }
    if let Some(branch) = branch {
        if !crate::cli::valid_model_id(branch) {
            return None;
        }
        if let Some(path) = fields.next() {
            if !codex_display_path(path) {
                return None;
            }
            if let Some(profile) = fields.next() {
                let profile = profile.strip_prefix("Main [")?.strip_suffix(']')?;
                if !crate::cli::valid_account_id(profile) || fields.next().is_some() {
                    return None;
                }
            }
        }
    }
    let model = match header.rsplit_once(' ') {
        Some((model, "low" | "medium" | "high" | "xhigh" | "max" | "ultra")) => model,
        _ => header,
    };
    (model
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && crate::cli::valid_model_id(model))
    .then_some(model)
}

fn codex_display_path(path: &str) -> bool {
    // Only display labels, never resolved or returned to the caller.
    if matches!(path, "~" | "/") {
        return true;
    }
    let Some(relative) = path.strip_prefix("~/").or_else(|| path.strip_prefix('/')) else {
        return false;
    };
    relative.len() <= 512
        && relative.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn observed_model<'a>(provider: &str, screen: &'a str) -> Option<&'a str> {
    if provider == "codex" {
        if screen.len() > MAX_CODEX_CAPTURE_BYTES {
            return None;
        }
        // Preserve the existing status-only lead layout only at the current
        // tail, never by looking past a malformed chrome or trailing text.
        let mut rows = screen.lines().rev().filter(|line| !line.trim().is_empty());
        let status = rows.next()?;
        if status.len() > MAX_CODEX_CHROME_ROW_BYTES
            || status.chars().any(char::is_control)
            || rows.next().is_some_and(codex_key_hint)
        {
            return None;
        }
        return codex_footer_model(status);
    }
    screen
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(6)
        .find_map(|line| {
            let line = line.trim();
            let model = match provider {
                "claude" if line.contains(" | ctx ") => {
                    let (model, fields) = line.split_once(" | ")?;
                    if let Some(version) = model.strip_prefix("Fable ") {
                        // A Fable prefix and a context-like substring can both occur in prose.
                        let (major, minor) = version.split_once('.')?;
                        static CONTEXT: std::sync::LazyLock<regex::Regex> =
                            std::sync::LazyLock::new(|| {
                                regex::Regex::new(concat!(
                                    r"^ctx (?:--|\[[#-]{1,10}\] (?:[0-9]|[1-9][0-9]|100)%",
                                    r"(?: [0-9]+(?:\.[0-9]+)?[kmb]?/[0-9]+(?:\.[0-9]+)?[kmb]?)?)$",
                                ))
                                .expect("static Claude context pattern")
                            });
                        if ![major, minor].iter().all(|part| {
                            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                        }) || !fields.split(" | ").any(|field| CONTEXT.is_match(field))
                        {
                            return None;
                        }
                    } else if !["Sonnet ", "Opus ", "Haiku "]
                        .iter()
                        .any(|prefix| model.starts_with(prefix))
                    {
                        return None;
                    }
                    model
                }
                "antigravity" if line.contains(" | ctx ") => {
                    line.split_once(" (Antigravity")?.0.trim_end()
                }
                _ => return None,
            };
            (!model.is_empty()
                && model.len() <= 80
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || " .-()_/".contains(c)))
            .then_some(model)
        })
}

pub static CHAT_SGR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\x1b\[[0-9;]{0,64}m").expect("static SGR pattern")
});

pub fn provider_name(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        _ => "AGY",
    }
}

fn binding_summary(
    binding: Option<&PersistedAccountBinding>,
) -> (Option<&str>, bool, AccountVerification) {
    match binding {
        Some(b) => (b.account_id.as_deref(), b.account_pinned, b.verification),
        None => (None, false, AccountVerification::Unverified),
    }
}

pub fn run(
    repo: &Path,
    provider: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    run_with_host(repo, provider, options, services, stop, &SystemAccountHost)
}

fn run_with_host(
    repo: &Path,
    provider: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
    host: &dyn AccountHost,
) -> Result<Output> {
    crate::parent_only()?;
    let room = commands::room(options).ok_or_else(|| Error::new("select the Main's room"))?;
    let store = Store::open(repo)?;
    let tx = store.transaction("panel-provider-sessions", Duration::from_secs(3))?;
    let panel = Panel::load(repo, &room, services, stop)?;
    tx.check()?;
    let mut launch = LeadLaunch {
        model: options.get("model"),
        effort: options.get("reasoning-effort"),
        permissions: None,
    };
    // Typed task binding covers only ordinary Codex Leads; other providers and
    // the alternate Codex keep their managed-dispatch contract unchanged.
    let typed = provider == "codex" && !options.has("lead") && !options.has("focus-orchestrator");
    if typed {
        native_result::guard_unbound(&panel, options)?;
    }
    let mut value = if options.has("lead") {
        let caller = AlternateCaller::from_env();
        require(
            options
                .get("owner")
                .is_none_or(|owner| owner == panel.owner),
            "the Codex alternate lead requires the room's current Main owner",
        )?;
        if options.has("focus-orchestrator") {
            panel.focus_codex_alternate(&caller, host, &tx)?
        } else {
            let alias = options.required("account")?;
            if let Some(task) = options.get("task") {
                panel.assign_codex_alternate(&caller, &alias, task, host, &tx)?
            } else {
                panel.launch_codex_alternate(&caller, &alias, launch, host, &tx)?
            }
        }
    } else if options.has("focus-orchestrator") {
        panel.focus(provider)?;
        let binding = panel
            .leads
            .get(provider)
            .and_then(|lead| panel.live_binding(provider, lead, host));
        let (account, account_pinned, account_verification) = binding_summary(binding.as_ref());
        json!({
            "status": "focused",
            "provider": provider,
            "account": account,
            "account_pinned": account_pinned,
            "account_verification": account_verification,
        })
    } else {
        let attempts_cache = AttemptCache::default();
        let attempts = panel.require_main_with_cache(options.get("owner"), &attempts_cache)?;
        if !panel.leads.contains_key(provider)
            && !options.has("task")
            && !options.has("native-result")
            && !options.has("native-reconcile")
            && !options.has("native-adopt")
        {
            let main = crate::rows(&attempts)
                .iter()
                .find(|row| row["attempt_id"] == panel.attempt)
                .ok_or_else(|| Error::new("live Main attempt unavailable"))?;
            launch.permissions = Some(lead_permissions(main)?);
        }
        tx.check()?;
        let panel = if typed
            && [
                "native-adopt",
                "native-reconcile",
                "native-result",
                "task-id",
            ]
            .iter()
            .any(|flag| options.has(flag))
        {
            panel.with_native_codex_candidate()?
        } else {
            panel
        };
        if options.has("native-adopt") {
            native_succession::adopt(&panel, options, host, &tx)?
        } else if options.has("native-reconcile") {
            native_result::reconcile(&panel, options, host, &attempts_cache, &tx)?
        } else if options.has("native-result") {
            native_assignment::register_result(&panel, options, host, &attempts_cache, &tx)?
        } else if options.has("task-id") {
            native_assignment::run(&panel, options, host, &attempts_cache, &tx)?
        } else if options.has("spread-accounts")
            && !panel.leads.contains_key(provider)
            && !options.has("task")
        {
            require(provider == "claude", "account spread requires Claude")?;
            panel.start_with_account(provider, LeadAccount::Spread(&attempts), launch, host)?
        } else {
            panel.execute_with_host_cached(
                provider,
                options.get("account"),
                options.get("task"),
                launch,
                LeadExecution {
                    host,
                    attempts: &attempts_cache,
                    transaction: Some(&tx),
                },
            )?
        }
    };
    if options.has("task") && !options.has("task-id") && (typed || options.has("unbound")) {
        value["task_binding"] = "unbound".into();
        value["task_completion"] = "unconfirmed".into();
        value["acceptance"] = "unsupported".into();
    }
    let code = if (options.has("task-id") || options.has("task")) && value["status"] == "unknown" {
        75
    } else {
        0
    };
    Ok((code, json(&value)?, vec![]))
}

fn claude_worker_load(attempts: &Value) -> BTreeMap<&str, usize> {
    let mut unique = BTreeMap::new();
    for row in crate::rows(attempts) {
        let id = text(row, "attempt_id");
        if valid_attempt(id) {
            unique
                .entry(id)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(row));
        }
    }
    let mut load = BTreeMap::new();
    for row in unique.into_values().flatten() {
        let refs = &row["refs"];
        if row["provider"] == "claude"
            && refs
                .get("panel_provider")
                .is_none_or(|provider| provider == "claude")
            && row["terminal"] == false
            && [
                "starting",
                "working",
                "running",
                "waiting_input",
                "waiting_approval",
                "verifying",
            ]
            .contains(&text(row, "state"))
            && [
                "peer-delegate",
                "peer-ask",
                "peer-review",
                "consult",
                "advise",
                "agent-call",
            ]
            .contains(&text(row, "tool"))
            && ["worker", "researcher", "advisor", "reviewer", "delegate"]
                .contains(&text(refs, "panel_role"))
            && let Some(alias) = text(refs, "panel_account").strip_prefix("claude:")
            && crate::cli::valid_account_id(alias)
        {
            *load.entry(alias).or_insert(0) += 1;
        }
    }
    load
}

/// Codex rows never carry parent-visible text. Claude rows keep it: the dock
/// shows it only under a parent-summary label, never as a child's own chat.
fn strip_native_answers(rows: &mut [Value]) {
    for row in rows {
        if let Some(row) = row.as_object_mut() {
            row.remove("native_answer");
        }
    }
}

/// A proven child the whole-file collector gave no row (an oversize lead) is
/// painted from its spawn description; a collector row for it always wins.
/// The Claude sighting's rows and locators, trusted only when the second exact
/// pane proof equals the first and every child names the scan's own Turn.
/// Otherwise nothing from the sighting is painted or bound. Usage and the proven
/// `started_at`/`ended_at` clocks go only on a child's single painted row.
fn claude_sighting<P: PartialEq>(
    mut rows: Vec<Value>,
    first: &P,
    second: Option<&P>,
    scan: Option<(String, Vec<crate::claude_child_chat::ClaudeChild>)>,
) -> (Vec<Value>, Vec<(String, NativeLocator)>) {
    let Some((turn, children)) = scan else {
        return (rows, Vec::new());
    };
    if second != Some(first) || children.iter().any(|child| child.locator.turn != turn) {
        return (rows, Vec::new());
    }
    merge_claude_rows(&mut rows, &children);
    for child in &children {
        let participant = crate::native_children::participant_id("claude", &child.locator.agent);
        let mut painted = rows
            .iter_mut()
            .filter(|row| row["participant"] == participant);
        if let (Some(usage), Some(row), None) = (&child.usage, painted.next(), painted.next()) {
            row["native_response_usage"] = claude_usage_field(usage);
            if let Some(model) = usage
                .response
                .as_ref()
                .and_then(|response| response.observed_model.as_deref())
                .and_then(crate::native_children::observed_model)
            {
                row["observed_model"] = json!(model);
            }
            // Only the proven spawn and delivered-notice clocks; never from `state`.
            for (key, at) in [
                ("started_at", &usage.spawned_at),
                ("ended_at", &usage.delivered_at),
            ] {
                if let Some(at) = at {
                    row[key] = json!(at);
                }
            }
        }
    }
    let bound = painted_claude_children(children, &rows).collect();
    (rows, bound)
}

/// A Board row's `native_response_usage`: one response's own counters, never
/// cumulative, the lead's or context occupancy; clocks as the lead recorded them.
pub(crate) fn claude_usage_field(usage: &crate::claude_child_chat::ClaudeChildUsage) -> Value {
    let response = usage.response.as_ref().and_then(|response| {
        let total = response
            .usage
            .iter()
            .try_fold(0u64, |sum, &n| sum.checked_add(n))?;
        let mut fields = json!({"message_id": response.id, "timestamp": response.at,
                                "total_tokens": total});
        for (key, count) in crate::claude_child_chat::USAGE_KEYS
            .iter()
            .zip(response.usage)
        {
            fields[*key] = json!(count);
        }
        Some(fields)
    });
    json!({"scope": "latest_child_response", "response": response,
           "spawned_at": usage.spawned_at, "delivered_at": usage.delivered_at})
}

/// Participants of `"antigravity"` `native_child` rows painted exactly once, in
/// participant order.
fn unique_agy_rows(rows: &[Value]) -> impl Iterator<Item = &str> {
    let mut row_counts = BTreeMap::<&str, usize>::new();
    for row in rows {
        if row["native_child"] == true
            && row["provider"] == "antigravity"
            && let Some(participant) = row["participant"].as_str()
            && !participant.is_empty()
        {
            *row_counts.entry(participant).or_default() += 1;
        }
    }
    row_counts
        .into_iter()
        .filter_map(|(participant, count)| (count == 1).then_some(participant))
}

/// Unique painted AGY rows whose child transcript one sample may read for usage.
const AGY_USAGE_ROWS: usize = 3;

/// Attach `native_response_usage` = `{"scope": "latest_child_response",
/// "response": <the AGY reader's latest-response object, counters verbatim>}`
/// to an AGY child's only row of any kind; AGY reports no total and none is
/// summed. Only the first [`AGY_USAGE_ROWS`] such participants in participant
/// order are offered to the single `scan`;
/// nothing is attached unless it names the shown Turn and `prove_again`
/// re-proves the same lead pane afterwards. With no such row, neither runs.
fn agy_usage_sighting(
    rows: &mut [Value],
    shown_turn: &str,
    scan: impl FnOnce(&BTreeSet<String>) -> Option<(String, BTreeMap<String, Value>)>,
    prove_again: impl FnOnce() -> bool,
) {
    let painted: BTreeSet<String> = unique_agy_rows(rows)
        .filter(|&p| rows.iter().filter(|row| row["participant"] == p).count() == 1)
        .take(AGY_USAGE_ROWS)
        .map(str::to_owned)
        .collect();
    if shown_turn.is_empty() || painted.is_empty() {
        return;
    }
    let Some((turn, usage)) = scan(&painted) else {
        return;
    };
    if turn != shown_turn || usage.is_empty() || !prove_again() {
        return;
    }
    for (participant, value) in usage {
        let mut named = rows
            .iter_mut()
            .filter(|row| row["participant"] == participant.as_str());
        if let (true, Some(row), None) =
            (painted.contains(&participant), named.next(), named.next())
        {
            if let Some(model) = value["model"]
                .as_str()
                .and_then(crate::native_children::observed_model)
            {
                row["observed_model"] = json!(model);
            }
            row["native_response_usage"] =
                json!({"scope": "latest_child_response", "response": value});
        }
    }
}

/// Bind unique painted `"antigravity"` `native_child` rows ONLY to the proven lead
/// process/pane/Turn; this binding itself never reads private AGY transcripts.
pub(crate) fn agy_sighting(
    rows: &[Value],
    shown_turn: &str,
    pid: u32,
    birth: &str,
) -> Vec<(String, NativeLocator)> {
    if shown_turn.is_empty() || pid == 0 || birth.is_empty() {
        return Vec::new();
    }
    unique_agy_rows(rows)
        .map(|participant| {
            (
                participant.to_owned(),
                NativeLocator::Antigravity(crate::agy_child_chat::AgySelectedChild {
                    pid,
                    birth: birth.to_owned(),
                    turn: shown_turn.to_owned(),
                    participant: participant.to_owned(),
                }),
            )
        })
        .collect()
}

pub(crate) fn merge_claude_rows(
    rows: &mut Vec<Value>,
    children: &[crate::claude_child_chat::ClaudeChild],
) {
    for child in children {
        let participant = crate::native_children::participant_id("claude", &child.locator.agent);
        if !rows.iter().any(|row| row["participant"] == participant) {
            rows.push(crate::native_children::claude_child_row(
                &child.locator.agent,
                &child.title,
                child.state,
            ));
        }
    }
}

/// Project display-only Claude background-agent summary rows from an owned pane
/// footer after `claude_sighting`. Keeps one row per distinct slug, deduplicates
/// against authority-backed rows only on proven participant identity, and never
/// binds `readings.chats` or grants detail transcript access.
fn project_claude_pane_roster(
    same_pane_process: bool,
    screen: &str,
    activity_present: bool,
    rows: &mut Vec<Value>,
    provider_activity: &mut Value,
) {
    if !same_pane_process {
        return;
    }
    let roster = crate::provider_activity::claude_roster(screen);
    if !activity_present && roster.iter().any(|row| row.state == "working") {
        provider_activity["claude"] = json!("working");
    }
    let existing = rows.len();
    let mut seen_slugs = BTreeSet::new();
    for roster_row in &roster {
        if !seen_slugs.insert(roster_row.slug.as_str()) {
            continue;
        }
        let participant = crate::native_children::participant_id("claude", &roster_row.slug);
        let duplicate = rows[..existing]
            .iter()
            .any(|row| row["participant"].as_str() == Some(participant.as_str()));
        if !duplicate {
            rows.push(crate::native_children::claude_child_row(
                &roster_row.slug,
                &roster_row.title,
                roster_row.state,
            ));
        }
    }
}

/// Only children painted as rows can be opened, under the row's own id.
pub(crate) fn painted_claude_children(
    children: Vec<crate::claude_child_chat::ClaudeChild>,
    rows: &[Value],
) -> impl Iterator<Item = (String, NativeLocator)> {
    children.into_iter().filter_map(|child| {
        let participant = crate::native_children::participant_id("claude", &child.locator.agent);
        rows.iter()
            .any(|row| row["participant"] == participant)
            .then_some((participant, NativeLocator::Claude(child.locator)))
    })
}

fn read_native_chat(
    repo: &Path,
    terminal: &str,
    locator: &NativeLocator,
    root: Option<&CodexChatRoot>,
    stop: &AtomicBool,
) -> Option<String> {
    match locator {
        NativeLocator::Codex(locator) => {
            let (home, fd) = root?.parts();
            crate::native_children::codex_chat(repo, terminal, locator, home, fd, stop)
        }
        NativeLocator::Claude(locator) => {
            crate::claude_child_chat::claude_child_chat(repo, &locator.turn, locator)
        }
        NativeLocator::Antigravity(_) => None,
    }
}

#[cfg(test)]
pub(crate) mod codex_chat_fixture {
    use super::*;
    use oms_panel_core::services::ExecutionRequest;
    use oms_panel_core::{CommandOutput, PeerExecutor};
    use std::cell::Cell;

    const ROOM: &str = "room-fixture";
    const MAIN: &str = "att_11111111111111111111111111111111";
    pub(crate) const TURN: &str = "0123456789abcdef0123";
    pub(crate) const NEW_TURN: &str = "abcdef0123456789abcd";

    pub(crate) struct BoardSpy {
        repo: PathBuf,
        stop: AtomicBool,
        account_host: account_binding::test_support::FakeHost,
        pub(crate) docked: Cell<bool>,
        pub(crate) main_active: Cell<bool>,
        pub(crate) room_closed: Cell<bool>,
        pub(crate) main_terminal: Cell<bool>,
        pub(crate) list_count: Cell<usize>,
        pub(crate) show_count: Cell<usize>,
        pub(crate) capture_count: Cell<usize>,
        pub(crate) lead: Cell<&'static str>,
        pub(crate) turn: Cell<&'static str>,
        pub(crate) provider: Cell<&'static str>,
        pub(crate) lead_pid: Cell<Option<u32>>,
    }

    impl BoardSpy {
        pub(crate) fn new(label: &str) -> Self {
            let repo = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("oms-board-chat-{}-{label}", std::process::id()));
            std::fs::create_dir(&repo).unwrap();
            std::fs::create_dir_all(repo.join(".oms/lifecycle")).unwrap();
            std::fs::write(
                repo.join(".oms/lifecycle/events.jsonl"),
                b"fixture ledger\n",
            )
            .unwrap();
            let mut account_host =
                account_binding::test_support::FakeHost::new(repo.join("fixture-state"));
            let pid = std::process::id();
            account_host
                .owners
                .insert(pid, terminals::process(pid).unwrap());
            let home = account_binding::test_support::private_dir(&repo.join("fixture-home"));
            account_host.active_homes.insert("codex".into(), home);
            Self {
                repo,
                stop: AtomicBool::new(false),
                account_host,
                docked: Cell::new(false),
                main_active: Cell::new(false),
                room_closed: Cell::new(false),
                main_terminal: Cell::new(false),
                list_count: Cell::new(0),
                show_count: Cell::new(0),
                capture_count: Cell::new(0),
                lead: Cell::new("%3"),
                turn: Cell::new(TURN),
                provider: Cell::new("codex"),
                lead_pid: Cell::new(None),
            }
        }

        pub(crate) fn seed_snapshot(&self, session: &commands::Session) {
            oms_panel_core::state::snapshot_with_cache(
                &self.repo,
                Some(ROOM),
                self,
                &self.stop,
                Some(session.attempt_cache()),
            )
            .unwrap();
        }

        pub(crate) fn ledger(&self) -> PathBuf {
            self.repo.join(".oms/lifecycle/events.jsonl")
        }

        pub(crate) fn panel(&self) -> Panel<'_> {
            let mut panel =
                Panel::load_for_caller(&self.repo, ROOM, "%2", self, &self.stop).unwrap();
            panel.chat_host = Some(&self.account_host);
            panel
        }

        pub(crate) fn main_panel(&self) -> Panel<'_> {
            Panel::load_for_caller(&self.repo, ROOM, "%1", self, &self.stop).unwrap()
        }

        pub(crate) fn observations(&self, participant: &str, tag: &str) -> Observations {
            Observations {
                chats: BTreeMap::from([(
                    participant.into(),
                    ChatBinding {
                        pane: self.panel().chat_pane("codex").unwrap(),
                        locator: NativeLocator::Codex(
                            crate::native_children::fixture_codex_locator(tag),
                        ),
                    },
                )]),
                ..Default::default()
            }
        }

        /// A Claude lead pane (set `provider` first) bound to a synthetic child of `turn`.
        pub(crate) fn claude_observations(&self, participant: &str, turn: &str) -> Observations {
            Observations {
                chats: BTreeMap::from([(
                    participant.into(),
                    ChatBinding {
                        pane: self.panel().chat_pane("claude").unwrap(),
                        locator: NativeLocator::Claude(self.claude_locator(turn)),
                    },
                )]),
                ..Default::default()
            }
        }

        pub(crate) fn claude_locator(
            &self,
            turn: &str,
        ) -> crate::claude_child_chat::ClaudeChildLocator {
            crate::claude_child_chat::ClaudeChildLocator {
                pid: std::process::id(),
                birth: "fixture-birth".into(),
                session: "0a1b2c3d-4e5f-6789-abcd-ef0123456789".into(),
                turn: turn.into(),
                agent: "a1b2c3d4e5f60718".into(),
                tool_use: "toolu_fixture".into(),
                boundary: "fixture-boundary".into(),
                lead: (0, 0),
                child: (0, 0),
            }
        }

        /// An AGY lead pane (set `provider` first) bound to a painted child of `turn`.
        pub(crate) fn agy_observations(&self, participant: &str, turn: &str) -> Observations {
            Observations {
                chats: BTreeMap::from([(
                    participant.into(),
                    ChatBinding {
                        pane: self.panel().chat_pane("antigravity").unwrap(),
                        locator: NativeLocator::Antigravity(self.agy_locator(participant, turn)),
                    },
                )]),
                ..Default::default()
            }
        }

        pub(crate) fn agy_locator(
            &self,
            participant: &str,
            turn: &str,
        ) -> crate::agy_child_chat::AgySelectedChild {
            let pid = std::process::id();
            let birth = terminals::process(pid).unwrap().birth;
            crate::agy_child_chat::AgySelectedChild {
                pid,
                birth,
                turn: turn.into(),
                participant: participant.into(),
            }
        }
    }

    impl Drop for BoardSpy {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.repo);
        }
    }

    fn output(stdout: impl Into<Vec<u8>>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.into(),
            cleanup_complete: true,
            output_complete: true,
            ..Default::default()
        }
    }

    impl PeerExecutor for BoardSpy {
        fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            let args: Vec<_> = request
                .argv
                .iter()
                .map(|word| word.to_str().unwrap())
                .collect();
            let pid = std::process::id();
            let body = match args.as_slice() {
                [
                    "tmux",
                    "list-panes",
                    "-s",
                    "-t",
                    "%1" | "%2",
                    "-F",
                    PANE_FORMAT,
                ] => {
                    let window = if self.docked.get() { "@1" } else { "@3" };
                    let lead = self.lead.get();
                    let main_active = u8::from(self.main_active.get());
                    let provider = self.provider.get();
                    let lead_pid = self.lead_pid.get().unwrap_or(pid);
                    format!(
                        "$1\t@1\t%1\t{pid}\t0\t1\t%1\t{MAIN}\tcodex\t{MAIN}\t{ROOM}\t\t\t{main_active}\t.\n\
                         $1\t{window}\t{lead}\t{lead_pid}\t0\t0\t\t\t\t\t{ROOM}\t{provider}\t{MAIN}\t0\t."
                    )
                }
                [
                    "tmux",
                    "display-message",
                    "-p",
                    "-t",
                    "$1",
                    terminals::PROVIDER_SESSION_IDENTITY_FORMAT,
                    ";",
                    "show-options",
                    "-v",
                    "-t",
                    "$1",
                    "@oms_panel_repo",
                ] => {
                    format!("$1\t{pid}\tfixture-socket\t.\n{}\n", self.repo.display())
                }
                ["tmux", "display-message", "-p", "-t", "%2", _] => format!(
                    "$1\t@1\t{pid}\t0\t1\t0\t1\t{MAIN}\t{ROOM}\t{}",
                    self.repo.display()
                ),
                ["tmux", "capture-pane", "-p", "-J", "-S", "-100", "-t", pane]
                    if *pane == self.lead.get() =>
                {
                    self.capture_count.set(self.capture_count.get() + 1);
                    format!(
                        "› Task from Overall Main. Work within this task and report the result here. Turn {}. You own this provider's work.",
                        self.turn.get()
                    )
                }
                _ => panic!("unexpected or mutating tmux call {args:?}"),
            };
            Ok(output(body))
        }
    }

    impl Services for BoardSpy {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            args: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            let body = match command {
                NativeCommand::Room => {
                    assert!(
                        args == ["show", "--id", ROOM, "--exact", "--json"]
                            || args == ["show", "--id", ROOM, "--json"]
                            || args
                                == [
                                    "show",
                                    "--repo",
                                    self.repo.to_str().unwrap(),
                                    "--id",
                                    ROOM,
                                    "--exact",
                                    "--json"
                                ]
                    );
                    json!({
                        "id": ROOM, "closed": self.room_closed.get(),
                        "participants": [{"participant": MAIN, "role": "main", "provider": "codex", "joined": true}]
                    })
                }
                NativeCommand::Events => {
                    let row = json!({
                        "attempt_id": MAIN, "tool": "panel-main", "provider": "codex",
                        "terminal": self.main_terminal.get(),
                        "state": if self.main_terminal.get() { "done" } else { "working" },
                        "refs": {"panel_role": "main", "panel_room_id": ROOM, "panel_native_scheduling": "unmanaged"}
                    });
                    if args == ["list", "--json"] {
                        self.list_count.set(self.list_count.get() + 1);
                        json!([row])
                    } else {
                        assert_eq!(args, &["show", "--attempt", MAIN, "--json"]);
                        self.show_count.set(self.show_count.get() + 1);
                        row
                    }
                }
                _ => panic!("unexpected or mutating service call {command:?}"),
            };
            Ok(output(serde_json::to_vec(&body).unwrap()))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        CHAT_SGR, LeadLaunch, PASTE_DEADLINE, POLL_STEP, Pace, Proof, STARTUP_PROMPT,
        SUBMIT_DEADLINE, codex_model_from_chrome, codex_turn_shown, input_ready, literal_tmux_arg,
        live_provider, observed_model, orchestrator_command, paste_and_submit, weekly_from_usage,
    };
    use crate::{Error, Result};
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    const CODEX_STATUS: &str = "\n\x1b[49m  \x1b[38;5;223mGPT-6.2-Sol high\x1b[0m · Context 100% left · weekly 48% left\n← for agents · ? for shortcuts";
    const CODEX_DISPLAY_STATUS: &str = "\n  GPT-6.2-Sol high · Context 9% left · weekly 13% left · main · ~/fixture · Main [default]\n← for agents · ? for shortcuts";
    const CODEX_EMPTY: &str =
        "\x1b[1m›\x1b[0m\x1b[48;5;235m \x1b[2mAsk Codex to do anything\x1b[0m\x1b[48;5;235m\n";
    const CODEX_DRAFT: &str = "\x1b[1m›\x1b[0m do the work\n";
    const CODEX_OPAQUE_PASTE: &str = "\x1b[1m›\x1b[0m [Pasted Content 1234 chars]\n";
    const TURN_ID: &str = concat!("abcde01234", "56789abcde");
    const OLD_TURN_ID: &str = concat!("0123456789", "fedcba9876");

    /// A submitted Codex user message whose turn turn_id wraps onto a second row.
    fn user_message(turn_id: &str) -> String {
        format!(
            "\x1b[1;2m› \x1b[0m\x1b[48;5;236mTask from Overall Main. Work here. Turn\n  {turn_id}. You own this provider's work.\x1b[0m\n\n• Working (0s)"
        )
    }

    fn polls(deadline: Duration) -> u32 {
        (deadline.as_millis() / POLL_STEP.as_millis()) as u32
    }

    fn ordinary_frame(pane: &str, screen: &str, width: usize, x_offset: usize) -> String {
        let plain = CHAT_SGR.replace_all(screen, "");
        let lines: Vec<_> = plain.lines().collect();
        let marker = lines
            .iter()
            .position(|line| line.starts_with('›'))
            .unwrap_or(0);
        let y = marker
            + lines[marker..]
                .iter()
                .take_while(|row| !row.trim().is_empty())
                .count()
                .saturating_sub(1);
        let x = crate::render::width(lines.get(y).unwrap_or(&"").trim_end()) + x_offset;
        let h = lines.len();
        format!("$1\t@1\t{pane}\t42\t0\t0\t1\t0\t{x}\t{y}\t{width}\t{h}\tcodex\t.\n{screen}")
    }

    /// Scripted tmux: each cursor read advances to the next screen, repeating
    /// the last; each call costs `cost` on the shared fake clock.
    struct FakePane {
        screens: Vec<String>,
        reads: usize,
        fail_paste: bool,
        cursor_at_marker: bool,
        draft_cursor_offset: usize,
        draft_width: usize,
        history: String,
        prompt_row: Option<&'static str>,
        cost: Duration,
        clock: Rc<Cell<Duration>>,
        calls: Vec<Vec<String>>,
    }

    impl FakePane {
        fn new(screens: &[&str]) -> Self {
            Self {
                screens: screens
                    .iter()
                    .map(|s| format!("{s}{CODEX_STATUS}"))
                    .collect(),
                reads: 0,
                fail_paste: false,
                cursor_at_marker: false,
                draft_cursor_offset: 0,
                draft_width: 1600,
                history: user_message(TURN_ID),
                prompt_row: None,
                cost: Duration::ZERO,
                clock: Rc::default(),
                calls: vec![],
            }
        }

        fn tmux(&mut self, args: &[&str]) -> Result<String> {
            self.clock.set(self.clock.get() + self.cost);
            self.calls
                .push(args.iter().map(|&a| a.to_owned()).collect());
            if args[0] == "display-message" {
                self.reads += 1;
            }
            let screen = &self.screens[self.reads.saturating_sub(1).min(self.screens.len() - 1)];
            let editable = screen.lines().next().unwrap_or_default();
            match args[0] {
                "display-message" if args.get(4) == Some(&super::ORDINARY_CODEX_DRAFT_FORMAT) => {
                    Ok(ordinary_frame(
                        args[3],
                        screen,
                        self.draft_width,
                        self.draft_cursor_offset,
                    ))
                }
                "display-message" if self.cursor_at_marker || screen.contains("Ask Codex") => {
                    Ok("0|2|5".into())
                }
                "display-message" => Ok("0|31|5".into()),
                "capture-pane" if args.contains(&"-J") => Ok(format!(
                    "{}\n{}",
                    self.history,
                    self.prompt_row.unwrap_or(editable)
                )),
                "capture-pane" if args.contains(&"-e") => Ok(screen.clone()),
                "capture-pane" => Ok(format!("history\n{}", CHAT_SGR.replace_all(editable, ""))),
                "paste-buffer" if self.fail_paste => Err(Error::new("paste failed")),
                _ => Ok(String::new()),
            }
        }

        fn sent(&self, verb: &str) -> Vec<&Vec<String>> {
            self.calls.iter().filter(|c| c[0] == verb).collect()
        }

        fn enters(&self) -> usize {
            self.sent("send-keys")
                .iter()
                .filter(|c| c.last().is_some_and(|k| k == "Enter"))
                .count()
        }
    }

    /// Runs one submission whose first `fences_ok` fence checks pass.
    fn submit(
        fake: &mut FakePane,
        prompt: &str,
        proof: Proof,
        fences_ok: usize,
    ) -> (Result<()>, u32) {
        let clock = fake.clock.clone();
        let base = Instant::now();
        let (mut sleeps, mut fences) = (0, 0);
        let result = paste_and_submit(
            "codex",
            "%7",
            prompt,
            proof,
            &mut |args| fake.tmux(args),
            &mut || {
                fences += 1;
                require_fence(fences <= fences_ok)
            },
            Pace {
                now: &mut || base + clock.get(),
                sleep: &mut |step| {
                    sleeps += 1;
                    clock.set(clock.get() + step);
                },
            },
        );
        (result.map(|_| ()), sleeps)
    }

    fn require_fence(ok: bool) -> Result<()> {
        if ok {
            Ok(())
        } else {
            Err(Error::new("provider or Main pane changed"))
        }
    }

    fn task(fake: &mut FakePane) -> (Result<()>, u32) {
        submit(fake, "do the work", Proof::Turn(TURN_ID), 2)
    }

    fn assert_unconfirmed(fake: &FakePane, result: Result<()>) {
        let error = result.unwrap_err();
        assert!(error.message.starts_with("task submission is unconfirmed"));
        assert!(error.publication_uncertain);
        assert_eq!(fake.enters(), 1);
        assert_eq!(fake.sent("paste-buffer").len(), 1);
    }

    #[test]
    fn task_is_pasted_once_and_confirmed_by_its_wrapped_turn_row() {
        let mut fake = FakePane::new(&[CODEX_DRAFT, CODEX_DRAFT, CODEX_DRAFT, CODEX_EMPTY]);
        let (result, sleeps) = task(&mut fake);
        result.unwrap();
        assert_eq!(sleeps, 1);
        let writes: Vec<_> = fake
            .calls
            .iter()
            .filter(|c| c[0] != "display-message" && c[0] != "capture-pane")
            .collect();
        let name = &writes[0][2];
        assert_eq!(name.len(), "oms-task-".len() + 16);
        assert_eq!(writes[0], &["set-buffer", "-b", name, "--", "do the work"]);
        assert_eq!(
            writes[1],
            &["paste-buffer", "-p", "-d", "-b", name, "-t", "%7"]
        );
        assert_eq!(writes[2], &["send-keys", "-t", "%7", "Enter"]);
        assert_eq!(writes.len(), 3);
        assert_eq!(
            fake.calls.last().unwrap(),
            &[
                "capture-pane",
                "-p",
                "-J",
                "-S",
                "-200",
                "-E",
                "5",
                "-t",
                "%7"
            ]
        );
    }

    #[test]
    fn codex_rows_lose_parent_text_and_claude_children_bind_only_painted_rows() {
        use super::{
            NativeLocator, merge_claude_rows, painted_claude_children, strip_native_answers,
        };
        use crate::claude_child_chat::ClaudeChild;
        use serde_json::json;
        let spy = super::codex_chat_fixture::BoardSpy::new("claude-rows");
        let mut codex = vec![
            json!({"participant": "native-codex-x", "native_child": true,
            "provider": "codex", "native_answer": "parent saw this"}),
        ];
        strip_native_answers(&mut codex);
        assert!(codex[0].get("native_answer").is_none() && codex[0]["native_child"] == true);

        // The call site strips Codex rows only: a Claude row keeps the parent summary,
        // which the dock shows under its own label and never as the child's chat.
        let rows = vec![json!({
            "participant": crate::native_children::participant_id("claude", "a1b2c3d4e5f60718"),
            "native_child": true, "provider": "claude", "native_answer": "parent saw this"
        })];
        let child = |agent: &str| {
            let mut locator = spy.claude_locator(super::codex_chat_fixture::TURN);
            locator.agent = agent.into();
            ClaudeChild {
                locator,
                title: "t".into(),
                state: "unknown",
                usage: None,
            }
        };
        let (painted, hidden) = (child("a1b2c3d4e5f60718"), child("f0e1d2c3b4a59687"));
        let bound: Vec<_> = painted_claude_children(vec![painted, hidden], &rows).collect();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].0, rows[0]["participant"]);
        assert!(matches!(bound[0].1, NativeLocator::Claude(_)));

        // Merging paints the proven sibling too, and never touches the existing row.
        let mut merged = rows.clone();
        let both = vec![child("a1b2c3d4e5f60718"), child("f0e1d2c3b4a59687")];
        merge_claude_rows(&mut merged, &both);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0], rows[0]);
        assert_eq!(painted_claude_children(both, &merged).count(), 2);
    }

    #[test]
    fn claude_sighting_is_trusted_only_under_one_pane_and_one_turn() {
        use super::claude_sighting;
        use crate::claude_child_chat::{ClaudeChild, ClaudeChildUsage, ClaudeResponse};
        use serde_json::json;
        let spy = super::codex_chat_fixture::BoardSpy::new("claude-sighting");
        let turn = super::codex_chat_fixture::TURN;
        let read = |usage| ClaudeChildUsage {
            response: Some(ClaudeResponse {
                id: "msg_1".into(),
                at: "2026-11-22T01:00:00Z".into(),
                usage,
                observed_model: Some("claude-opus-5-5".into()),
            }),
            spawned_at: Some("2026-11-22T00:59:00Z".into()),
            delivered_at: None,
        };
        let child = |turn: &str, state| ClaudeChild {
            locator: spy.claude_locator(turn),
            title: "t".into(),
            state,
            usage: Some(read([1, 2, 3, 4])),
        };
        let collector = vec![json!({"participant": "native-claude-other", "state": "working"})];
        let scan = |other: &str| {
            Some((
                turn.to_owned(),
                vec![child(turn, "done"), child(other, "unknown")],
            ))
        };
        let single = || Some((turn.to_owned(), vec![child(turn, "done")]));

        let (rows, bound) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), single());
        assert!(rows.len() == 2 && rows[1]["state"] == "done" && bound.len() == 1);
        let field = json!({"scope": "latest_child_response", "response": {"message_id": "msg_1",
            "timestamp": "2026-11-22T01:00:00Z", "input_tokens": 1, "output_tokens": 2,
            "cache_creation_input_tokens": 3, "cache_read_input_tokens": 4, "total_tokens": 10},
            "spawned_at": "2026-11-22T00:59:00Z", "delivered_at": null});
        assert_eq!(rows[1]["native_response_usage"], field);
        assert!(rows[0].get("native_response_usage").is_none());
        // The exact reply model is its own row key, never inside the usage object.
        assert_eq!(rows[1]["observed_model"], "claude-opus-5-5");
        assert!(rows[0].get("observed_model").is_none());
        assert!(field["response"].get("observed_model").is_none());
        let mut unseen = read([1, 2, 3, 4]);
        unseen.response.as_mut().unwrap().observed_model = None;
        let no_model = Some((
            turn.to_owned(),
            vec![ClaudeChild {
                usage: Some(unseen),
                ..child(turn, "done")
            }],
        ));
        let (rows_nm, _) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), no_model);
        assert!(
            rows_nm[1].get("observed_model").is_none()
                && rows_nm[1].get("native_response_usage").is_some()
        );
        // Row clocks come from the proven spawn and delivered notice, never the state.
        let clocks = |row: &serde_json::Value| {
            (row.get("started_at").cloned(), row.get("ended_at").cloned())
        };
        let spawned = json!("2026-11-22T00:59:00Z");
        assert_eq!(clocks(&rows[1]), (Some(spawned.clone()), None));
        assert_eq!(clocks(&rows[0]), (None, None));
        // A collector row is the child's single row and carries it the same way.
        let own = json!({"participant": bound[0].0, "state": "working"});
        let (rows, _) = claude_sighting(vec![own.clone()], &"pane", Some(&"pane"), single());
        assert!(rows.len() == 1 && rows[0]["native_response_usage"] == field);
        // Two rows of one participant stay bound, but neither gets usage or a model.
        let (rows, bound) = claude_sighting(vec![own.clone(); 2], &"pane", Some(&"pane"), single());
        assert!(rows == vec![own.clone(); 2] && bound.len() == 1);
        assert!(rows.iter().all(|row| row.get("observed_model").is_none()));
        // Usage withheld (over the tail budget or unproven) leaves the field absent;
        // a total past u64 is unknown, the clocks stay.
        let with = |usage| {
            let mut child = child(turn, "done");
            child.usage = usage;
            Some((turn.to_owned(), vec![child]))
        };
        let (rows, bound) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), with(None));
        assert!(
            rows.len() == 2 && rows[1].get("native_response_usage").is_none() && bound.len() == 1
        );
        assert!(rows[1].get("observed_model").is_none());
        assert_eq!(clocks(&rows[1]), (None, None));
        let clocked = |state, delivered: Option<&str>| {
            let mut usage = read([1, 2, 3, 4]);
            usage.delivered_at = delivered.map(str::to_owned);
            with(Some(usage)).map(|(turn, mut children)| {
                children[0].state = state;
                (turn, children)
            })
        };
        let delivered = json!("2026-11-22T01:02:00Z");
        let done = clocked("done", Some("2026-11-22T01:02:00Z"));
        let (rows, _) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), done);
        assert_eq!(clocks(&rows[1]), (Some(spawned.clone()), Some(delivered)));
        let running = clocked("working", None);
        let (rows, _) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), running);
        assert_eq!(clocks(&rows[1]), (Some(spawned.clone()), None));
        let over = with(Some(read([u64::MAX, 1, 0, 0])));
        let (rows, _) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), over);
        let mut unknown = field.clone();
        unknown["response"] = serde_json::Value::Null;
        assert_eq!(rows[1]["native_response_usage"], unknown);
        // A second proof that differs or is missing drops rows and locators.
        for second in [Some(&"moved"), None] {
            let (rows, bound) = claude_sighting(collector.clone(), &"pane", second, single());
            assert!(rows == collector && bound.is_empty());
        }
        // One child naming another Turn discards the whole sighting.
        let (rows, bound) = claude_sighting(
            collector.clone(),
            &"pane",
            Some(&"pane"),
            scan("0000000000000000dead"),
        );
        assert!(rows == collector && bound.is_empty());
        let (rows, bound) = claude_sighting(collector.clone(), &"pane", Some(&"pane"), None);
        assert!(rows == collector && bound.is_empty());
    }

    #[test]
    fn claude_pane_roster_projects_owned_summary_rows_without_chat_binding() {
        use super::{claude_sighting, project_claude_pane_roster};
        use crate::claude_child_chat::ClaudeChild;
        use serde_json::json;

        let ruler = "─".repeat(80);
        let same_title_screen = format!(
            "prior\n{ruler}\n❯\u{00a0}\n{ruler}\n  Opus 5.5 | proj | git main | ctx [##--] 21%\n  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 2 agents\n\n  ● main\n  ◯ worker-a  Audit panel  9m 49s · ↓ 218.9k tokens\n  ◯ worker-b  Audit panel  idle\n  ◯ worker-a  Audit panel  9m 50s · ↓ 219.0k tokens\n"
        );
        let spy = super::codex_chat_fixture::BoardSpy::new("claude-pane-roster");
        let turn = super::codex_chat_fixture::TURN;
        let transcript_child = ClaudeChild {
            locator: spy.claude_locator(turn),
            title: "Audit panel".into(),
            state: "working",
            usage: None,
        };

        // 1. Two distinct live slugs with the same title both project with distinct
        // participant IDs; duplicate slug in roster collapses; an authority-backed row
        // with only a matching title (different participant ID) does not suppress them;
        // transcript sighting keeps sole authority over chat bindings.
        let (mut rows, bound) = claude_sighting(
            Vec::new(),
            &"pane",
            Some(&"pane"),
            Some((turn.to_owned(), vec![transcript_child])),
        );
        let mut activity = json!({});
        project_claude_pane_roster(true, &same_title_screen, false, &mut rows, &mut activity);
        let worker_a = crate::native_children::participant_id("claude", "worker-a");
        let worker_b = crate::native_children::participant_id("claude", "worker-b");
        assert_ne!(worker_a, worker_b);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["title"], "Audit panel");
        assert_eq!(rows[1]["participant"], worker_a);
        assert_eq!(rows[1]["title"], "Audit panel");
        assert_eq!(rows[1]["state"], "working");
        assert_eq!(rows[2]["participant"], worker_b);
        assert_eq!(rows[2]["title"], "Audit panel");
        assert_eq!(rows[2]["state"], "unknown");
        assert_eq!(activity["claude"], "working");
        assert_eq!(bound.len(), 1);
        assert_ne!(bound[0].0, worker_a);
        assert_ne!(bound[0].0, worker_b);

        // 2. Authority-backed row with matching proven participant ID is deduplicated,
        // and liveness is omitted when all roster rows are idle.
        let idle_only = format!(
            "prior\n{ruler}\n❯\u{00a0}\n{ruler}\n  Opus 5.5 | proj | git main | ctx [##--] 21%\n  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent\n\n  ● main\n  ◯ builder-gh-templates  Build issue and PR templates  idle\n"
        );
        let builder_participant =
            crate::native_children::participant_id("claude", "builder-gh-templates");
        let mut dedup_rows = vec![json!({
            "participant": builder_participant,
            "title": "Other title",
            "state": "done"
        })];
        let mut idle_activity = json!({});
        project_claude_pane_roster(true, &idle_only, false, &mut dedup_rows, &mut idle_activity);
        assert_eq!(dedup_rows.len(), 1);
        assert!(idle_activity.get("claude").is_none());

        // 3. Unowned or fenced-out pane projects nothing and sets no activity.
        let mut unowned_rows = Vec::new();
        let mut unowned_activity = json!({});
        project_claude_pane_roster(
            false,
            &same_title_screen,
            false,
            &mut unowned_rows,
            &mut unowned_activity,
        );
        assert!(unowned_rows.is_empty());
        assert!(unowned_activity.get("claude").is_none());
    }

    fn lead_frame(pane: &str, body: &str, x: usize, y: usize, columns: usize) -> String {
        format!(
            "{pane}\t0\t0\t{x}\t{y}\t{columns}\t90\t.\n{}\n.",
            body.trim_end_matches('\n')
        )
    }

    fn lead_box(draft: &str) -> String {
        let border = "─".repeat(20);
        format!("{border}\n{draft}\n{border}")
    }

    #[test]
    fn ordinary_lead_delivery_only_joins_tmux_proven_wrapped_drafts() {
        let intended = "01234567890123456 789abcdef";
        let physical = lead_box("> 01234567890123456\n789abcdef");
        for (joined, cursor, verified) in [
            ("> 01234567890123456 789abcdef", (9, 2), true),
            ("> 01234567890123456\n789abcdef", (9, 2), false),
            ("> 01234567890123456789abcdef", (9, 2), false),
            ("> 01234567890123456 789abcdef", (8, 2), false),
        ] {
            let mut captures = 0;
            let result = super::lead_delivery::Frame::exact_draft(
                &mut |args| {
                    captures += 1;
                    let body = if args.contains(&"-J") {
                        assert!(
                            args.windows(5)
                                .any(|args| args == ["-J", "-S", "1", "-E", "2"])
                        );
                        joined
                    } else {
                        physical.as_str()
                    };
                    Ok(lead_frame("%7", body, cursor.0, cursor.1, 20))
                },
                "%7",
                "antigravity",
                intended,
            );
            assert_eq!(result.is_some(), verified, "{cursor:?}");
            assert_eq!(captures, 2);
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn check_ordinary_agy_delivery_geometry(
        turn: &str,
        record_user_turn: impl Fn(&str),
        prove_user_turn: impl Fn(&str) -> bool,
    ) {
        use super::lead_delivery::Delivery;
        let prompt = super::native_lead_prompt(
            turn,
            STARTUP_PROMPT.trim(),
            "",
            "Verify the assigned geometry fixture and report the evidence.",
        );
        let wire = format!("> {prompt}");
        let header = format!(
            "> {} Turn {turn}. You own this provider's work.",
            crate::native_children::TASK_PREFIX
        );
        assert!(wire.is_ascii() && header.len() > 126);
        let height = 21;
        for (columns, cropped) in [(126, false), (70, true)] {
            let rule = "─".repeat(columns);
            let rows: Vec<_> = wire
                .as_bytes()
                .chunks(columns)
                .map(|part| std::str::from_utf8(part).unwrap().trim_end_matches(' '))
                .collect();
            assert_eq!(rows.len() + 2 > height, cropped);
            let (screen, cursor_row) = if cropped {
                let tail = rows[rows.len() - (height - 2)..].join("\n");
                (format!("{tail}\n{rule}\nOrchestrator (Carbon)"), height - 3)
            } else {
                (format!("{rule}\n{}\n{rule}", rows.join("\n")), rows.len())
            };
            let cleared = format!(
                "The provider accepted a request.\n{rule}\n>\n{rule}\nOrchestrator (Carbon)"
            );
            let cursor_x = wire.len() % columns;
            assert!(cursor_x > 0);
            let captured = |body, x, y| {
                Ok(format!(
                    "%7\t0\t0\t{x}\t{y}\t{columns}\t{height}\t.\n{body}\n."
                ))
            };
            let start = Instant::now();
            let elapsed = Cell::new(Duration::ZERO);
            let pastes = Cell::new(0);
            let enters = Cell::new(0);
            let reads = Cell::new(0);
            let mut fences = 0;
            let outcome = paste_and_submit(
                "antigravity",
                "%7",
                &prompt,
                Proof::LeadTurn {
                    turn,
                    agy_user_turn: &|| {
                        reads.set(reads.get() + 1);
                        prove_user_turn(&prompt)
                    },
                },
                &mut |args| match args {
                    ["display-message", _, _, _, _, ";", ..] if enters.get() > 0 => {
                        captured(&cleared, 2, 2)
                    }
                    ["display-message", _, _, _, _, ";", ..] if pastes.get() == 0 => {
                        captured(&cleared, 2, 2)
                    }
                    ["display-message", _, _, _, _, ";", ..] if args.contains(&"-J") => {
                        assert!(!cropped, "cropped composer must not expose a joined draft");
                        captured(&wire, cursor_x, cursor_row)
                    }
                    ["display-message", _, _, _, _, ";", ..] => {
                        captured(&screen, cursor_x, cursor_row)
                    }
                    ["set-buffer", ..] => Ok(String::new()),
                    ["paste-buffer", ..] => {
                        pastes.set(pastes.get() + 1);
                        Ok(String::new())
                    }
                    ["send-keys", _, _, "Enter"] => {
                        assert!(!cropped);
                        enters.set(enters.get() + 1);
                        record_user_turn(&prompt);
                        Ok(String::new())
                    }
                    _ => panic!("unexpected synthetic tmux call"),
                },
                &mut || {
                    fences += 1;
                    Ok(())
                },
                Pace {
                    now: &mut || start + elapsed.get(),
                    sleep: &mut |step| elapsed.set(elapsed.get() + step),
                },
            )
            .unwrap();
            assert_eq!(pastes.get(), 1);
            if cropped {
                assert_eq!((enters.get(), reads.get(), fences), (0, 0, 1));
                assert_eq!(outcome.delivery, Delivery::Unverified);
                assert!(outcome.reconcile_cue().contains("Enter was withheld"));
            } else {
                assert_eq!((enters.get(), reads.get(), fences), (1, 1, 4));
                assert_eq!(outcome.delivery, Delivery::TurnObserved);
            }
        }
    }

    #[test]
    fn ordinary_lead_delivery_checks_the_exact_post_enter_evidence() {
        use super::lead_delivery::Delivery;
        use crate::native_children::TASK_PREFIX;

        let user = |mark, turn| {
            format!(
                "{mark} {TASK_PREFIX} Turn {turn}. You own this provider's work. Task: inspect the fixture"
            )
        };
        let agy = user('>', TURN_ID);
        let old = user('>', OLD_TURN_ID);
        let claude = user('❯', TURN_ID);
        let turn = format!("{agy}\n\nThe native response\n\n>\n");
        let old_latest = format!("{agy}\n\n{old}\n\n>\n");
        let quoted_turn = format!("{agy}\n\n>\n");
        let claude_turn = format!("{claude}\n\n❯\n");
        let agy_draft = lead_box(&agy);
        let claude_draft = lead_box(&claude);
        for (provider, before, after, cursor, fences_ok, role_proven, role_checks, expected) in [
            (
                "antigravity",
                ">",
                ">\n",
                (2, 0),
                4,
                true,
                1,
                Delivery::TurnObserved,
            ),
            (
                "antigravity",
                ">",
                old_latest.as_str(),
                (2, 4),
                4,
                false,
                1,
                Delivery::InputClearedUnverified,
            ),
            (
                "antigravity",
                turn.as_str(),
                turn.as_str(),
                (2, 4),
                4,
                true,
                0,
                Delivery::InputClearedUnverified,
            ),
            (
                "antigravity",
                ">",
                turn.as_str(),
                (2, 4),
                4,
                true,
                1,
                Delivery::TurnObserved,
            ),
            (
                "antigravity",
                ">",
                quoted_turn.as_str(),
                (2, 2),
                4,
                false,
                1,
                Delivery::InputClearedUnverified,
            ),
            (
                "antigravity",
                ">",
                agy_draft.as_str(),
                (crate::render::width(&agy), 1),
                4,
                false,
                0,
                Delivery::DraftRetained,
            ),
            (
                "antigravity",
                ">",
                turn.as_str(),
                (2, 4),
                2,
                true,
                0,
                Delivery::Unverified,
            ),
            (
                "antigravity",
                ">",
                turn.as_str(),
                (2, 4),
                3,
                true,
                1,
                Delivery::Unverified,
            ),
            (
                "claude",
                "❯",
                claude_turn.as_str(),
                (2, 2),
                4,
                true,
                0,
                Delivery::InputClearedUnverified,
            ),
            (
                "claude",
                "❯",
                claude_draft.as_str(),
                (crate::render::width(&claude), 1),
                4,
                false,
                0,
                Delivery::DraftRetained,
            ),
        ] {
            let start = Instant::now();
            let elapsed = Cell::new(Duration::ZERO);
            let enters = Cell::new(0);
            let pastes = Cell::new(0);
            let (draft, row) = if provider == "claude" {
                (&claude_draft, &claude)
            } else {
                (&agy_draft, &agy)
            };
            let before_row = before.lines().count() - 1;
            let role_reads = Cell::new(0);
            let mut fences = 0;
            let result = paste_and_submit(
                provider,
                "%7",
                &row[row.find(' ').unwrap() + 1..],
                Proof::LeadTurn {
                    turn: TURN_ID,
                    agy_user_turn: &|| {
                        role_reads.set(role_reads.get() + 1);
                        role_proven
                    },
                },
                &mut |args| match args {
                    ["display-message", _, _, _, _, ";", ..] => {
                        let ((x, y), body) = if enters.get() != 0 {
                            (cursor, after)
                        } else if pastes.get() != 0 {
                            ((crate::render::width(row), 1), draft.as_str())
                        } else {
                            ((2, before_row), before)
                        };
                        let body = if args.contains(&"-J") {
                            assert_eq!(y, 1);
                            body.lines().nth(1).unwrap()
                        } else {
                            body
                        };
                        Ok(lead_frame("%7", body, x, y, 160))
                    }
                    ["paste-buffer", ..] => {
                        pastes.set(pastes.get() + 1);
                        Ok(String::new())
                    }
                    ["send-keys", _, _, "Enter"] => {
                        enters.set(enters.get() + 1);
                        Ok(String::new())
                    }
                    ["set-buffer", ..] => Ok(String::new()),
                    _ => panic!("unexpected fake tmux route"),
                },
                &mut || {
                    fences += 1;
                    require_fence(fences <= fences_ok)
                },
                Pace {
                    now: &mut || start + elapsed.get(),
                    sleep: &mut |step| elapsed.set(elapsed.get() + step),
                },
            )
            .unwrap();
            assert_eq!(result.delivery, expected, "{provider} {cursor:?}");
            assert_eq!((pastes.get(), enters.get()), (1, 1));
            assert_eq!(role_reads.get(), role_checks);
            if expected == Delivery::Unverified {
                assert!(
                    result
                        .reconcile_cue()
                        .contains("custody changed after Enter")
                );
            }
        }
    }

    #[test]
    fn ordinary_lead_delivery_main_authority_tracks_room_and_lifecycle_changes() {
        use super::codex_chat_fixture::BoardSpy;
        use oms_panel_core::state::AttemptCache;

        let observed = BoardSpy::new("delivery-authority");
        let attempts = AttemptCache::default();
        let main = observed.main_panel();
        main.require_lead_assignment_main(&attempts).unwrap();
        observed.room_closed.set(true);
        assert!(
            main.require_lead_assignment_main(&attempts)
                .unwrap_err()
                .message
                .contains("membership")
        );
        observed.room_closed.set(false);
        main.require_lead_assignment_main(&attempts).unwrap();
        observed.main_terminal.set(true);
        std::fs::write(observed.ledger(), b"fixture ledger generation changed\n").unwrap();
        assert!(
            main.require_lead_assignment_main(&attempts)
                .unwrap_err()
                .message
                .contains("live Main attempt")
        );
        assert!(
            observed
                .panel()
                .require_lead_assignment_main(&attempts)
                .is_err()
        );
    }

    #[test]
    fn ordinary_lead_delivery_transport_errors_and_fence_loss_stay_uncertain() {
        use super::lead_delivery::Delivery;

        let header = format!(
            "{} Turn {TURN_ID}. You own this provider's work.",
            crate::native_children::TASK_PREFIX
        );
        let expected = format!("{header} Task: inspect");
        let intended = format!("❯ {expected}");
        let correct = lead_box(&intended);
        let wrong = lead_box(&format!("❯ {header} Task: replace"));
        let partial = lead_box(&format!("❯ {header}"));
        let extra = lead_box(&format!("{intended}\n  extra editable input"));
        for (failure, expected_paste, expected_enter, cue) in [
            ("before-paste", 0, 0, "before pasting"),
            ("paste", 1, 0, "Paste outcome is unknown"),
            ("unseen-draft", 1, 0, "draft could not be verified"),
            ("wrong-draft", 1, 0, "draft could not be verified"),
            ("marker-only", 1, 0, "draft could not be verified"),
            ("extra-input", 1, 0, "draft could not be verified"),
            ("trailing-input", 1, 0, "draft could not be verified"),
            ("room-after-paste", 1, 0, "custody changed after pasting"),
            ("changed-draft", 1, 0, "draft changed"),
            ("enter", 1, 1, "Enter may have reached"),
            ("room-after-enter", 1, 1, "custody changed after Enter"),
        ] {
            let start = Instant::now();
            let elapsed = Cell::new(Duration::ZERO);
            let pastes = Cell::new(0);
            let enters = Cell::new(0);
            let reads = Cell::new(0);
            let mut fences = 0;
            let result = paste_and_submit(
                "claude",
                "%7",
                &expected,
                Proof::LeadTurn {
                    turn: TURN_ID,
                    agy_user_turn: &|| panic!("Claude cannot read AGY evidence"),
                },
                &mut |args| match args {
                    ["display-message", _, _, _, _, ";", ..] if enters.get() > 0 => {
                        Ok(lead_frame("%7", "❯", 2, 0, 160))
                    }
                    ["display-message", _, _, _, _, ";", ..] if args.contains(&"-J") => {
                        let body = if failure == "trailing-input" {
                            format!("{intended} ")
                        } else {
                            match failure {
                                "wrong-draft" => wrong.lines().nth(1).unwrap(),
                                "marker-only" => partial.lines().nth(1).unwrap(),
                                _ => &intended,
                            }
                            .to_owned()
                        };
                        let x = if failure == "trailing-input" {
                            crate::render::width(&intended)
                        } else {
                            crate::render::width(&body)
                        };
                        Ok(lead_frame("%7", &body, x, 1, 160))
                    }
                    ["display-message", _, _, _, _, ";", ..] => {
                        reads.set(reads.get() + 1);
                        let screen = match failure {
                            "unseen-draft" => return Ok(lead_frame("%7", "❯", 2, 0, 160)),
                            "wrong-draft" => &wrong,
                            "marker-only" => &partial,
                            "extra-input" => &extra,
                            "changed-draft" if reads.get() == 3 => &wrong,
                            _ => &correct,
                        };
                        let row = screen.lines().nth(1).unwrap();
                        Ok(lead_frame("%7", screen, crate::render::width(row), 1, 160))
                    }
                    ["paste-buffer", ..] => {
                        pastes.set(pastes.get() + 1);
                        require_fence(failure != "paste").map(|()| String::new())
                    }
                    ["send-keys", _, _, "Enter"] => {
                        enters.set(enters.get() + 1);
                        require_fence(failure != "enter").map(|()| String::new())
                    }
                    ["set-buffer" | "delete-buffer", ..] => Ok(String::new()),
                    _ => panic!("unexpected fake tmux route"),
                },
                &mut || {
                    fences += 1;
                    if fences == 2 {
                        assert_eq!(reads.get(), 3, "custody must follow final draft capture");
                    }
                    require_fence(!matches!(
                        (failure, fences),
                        ("before-paste", 1) | ("room-after-paste", 2) | ("room-after-enter", 3)
                    ))
                },
                Pace {
                    now: &mut || start + elapsed.get(),
                    sleep: &mut |step| elapsed.set(elapsed.get() + step),
                },
            );
            assert_eq!(
                (pastes.get(), enters.get()),
                (expected_paste, expected_enter)
            );
            if failure == "before-paste" {
                assert!(result.unwrap_err().message.contains(cue));
            } else {
                let outcome = result.unwrap();
                assert_eq!(outcome.delivery, Delivery::Unverified);
                assert!(outcome.reconcile_cue().contains(cue), "{failure}");
            }
        }
    }

    #[test]
    fn empty_codex_input_without_the_turn_row_is_unconfirmed() {
        for history in [user_message(OLD_TURN_ID), String::new()] {
            let mut fake = FakePane::new(&[CODEX_DRAFT, CODEX_DRAFT, CODEX_DRAFT, CODEX_EMPTY]);
            fake.history = history;
            let (result, sleeps) = task(&mut fake);
            assert_unconfirmed(&fake, result);
            assert_eq!(sleeps, 1 + polls(SUBMIT_DEADLINE));
        }
    }

    #[test]
    fn turn_in_the_editable_prompt_row_is_not_a_submission() {
        let mut fake = FakePane::new(&[CODEX_DRAFT, CODEX_DRAFT, CODEX_DRAFT, CODEX_EMPTY]);
        fake.history = user_message(OLD_TURN_ID);
        fake.prompt_row = Some("› Task from Overall Main. Turn bfa1177f8c289615d17e. Task: x");
        let result = task(&mut fake).0;
        assert_unconfirmed(&fake, result);

        let turn = format!("Turn {TURN_ID}");
        let older = user_message(OLD_TURN_ID);
        assert!(codex_turn_shown(
            &format!("{}\n{CODEX_EMPTY}", user_message(TURN_ID)),
            &turn
        ));
        assert!(!codex_turn_shown(&format!("{older}\n› {turn}"), &turn));
        assert!(!codex_turn_shown(&format!("• Ran {turn}\n› Ask"), &turn));
        // A capture that no longer ends at the prompt row proves nothing.
        assert!(!codex_turn_shown(&user_message(TURN_ID), &turn));
        assert!(!codex_turn_shown(
            &format!("{}\n{CODEX_EMPTY}", user_message(&format!("{TURN_ID}0"))),
            &turn
        ));
        assert!(!codex_turn_shown(
            &format!(
                "{}\n{}\n{CODEX_EMPTY}",
                user_message(TURN_ID),
                user_message(OLD_TURN_ID)
            ),
            &turn
        ));
    }

    #[test]
    fn swallowed_enter_leaves_the_draft_and_is_never_resent() {
        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        let (result, sleeps) = task(&mut fake);
        assert_unconfirmed(&fake, result);
        assert_eq!(sleeps, 1 + polls(SUBMIT_DEADLINE));

        // The swallowed Enter may leave the cursor where an empty input would put it.
        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        fake.cursor_at_marker = true;
        let result = task(&mut fake).0;
        assert_unconfirmed(&fake, result);
    }

    #[test]
    fn dispatched_command_returns_after_its_single_enter() {
        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        let (result, sleeps) = submit(&mut fake, "/usage", Proof::Dispatched, 2);
        result.unwrap();
        assert_eq!((sleeps, fake.enters()), (1, 1));
        assert_eq!(
            fake.calls.last().unwrap(),
            &["send-keys", "-t", "%7", "Enter"]
        );
    }

    #[test]
    fn unobserved_paste_is_never_submitted() {
        let mut fake = FakePane::new(&[CODEX_EMPTY]);
        let (result, sleeps) = task(&mut fake);
        let error = result.unwrap_err();
        assert!(error.message.starts_with("task paste was not observed"));
        assert!(error.publication_uncertain);
        assert_eq!(sleeps, polls(PASTE_DEADLINE));
        assert_eq!(fake.enters(), 0);
        assert!(fake.sent("delete-buffer").is_empty());
        assert!(fake.sent("paste-buffer")[0].contains(&"-d".to_owned()));

        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        fake.fail_paste = true;
        let (result, sleeps) = task(&mut fake);
        assert_eq!(result.unwrap_err().message, "paste failed");
        assert_eq!((sleeps, fake.enters()), (0, 0));
        let name = &fake.sent("set-buffer")[0][2];
        assert_eq!(
            fake.sent("delete-buffer"),
            [&vec!["delete-buffer".to_owned(), "-b".into(), name.clone()]]
        );

        for placeholder in [
            CODEX_OPAQUE_PASTE,
            "\x1b[1m›\x1b[0m [Pasted Content 1560 chars]\n",
            "\x1b[1m›\x1b[0m [Pasted Content\n  1560 chars]\n",
        ] {
            let mut fake = FakePane::new(&[placeholder, placeholder, CODEX_EMPTY]);
            let (result, sleeps) = task(&mut fake);
            let error = result.unwrap_err();
            assert!(error.publication_uncertain);
            assert!(error.message.contains("Enter was withheld after pasting"));
            assert_eq!(sleeps, 1);
            assert_eq!(fake.enters(), 0);
            assert_eq!(fake.sent("paste-buffer").len(), 1);
        }

        let visible_long = format!(
            "\x1b[1m›\x1b[0m Task from Overall Main. [Pasted Content 1560 chars] {}\n",
            "x".repeat(1500)
        );
        let mut fake = FakePane::new(&[&visible_long, &visible_long, &visible_long, CODEX_EMPTY]);
        let intended = CHAT_SGR.replace_all(&visible_long, "");
        let (result, _) = submit(
            &mut fake,
            intended.trim_start_matches("› ").trim_end(),
            Proof::Turn(TURN_ID),
            2,
        );
        result.unwrap();
        assert_eq!(fake.enters(), 1);
    }

    #[test]
    fn deadlines_bound_elapsed_time_not_poll_counts() {
        let mut fake = FakePane::new(&[CODEX_EMPTY]);
        fake.cost = Duration::from_millis(300);
        let error = task(&mut fake).0.unwrap_err();
        assert!(error.message.starts_with("task paste was not observed"));
        // Six atomic 0.3s samples with 0.1s sleeps span the 2s paste wait.
        assert_eq!(fake.reads, 6);
        assert_eq!(fake.clock.get(), Duration::from_millis(2900));

        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        fake.cost = Duration::from_millis(400);
        let (result, sleeps) = task(&mut fake);
        assert_unconfirmed(&fake, result);
        assert_eq!(sleeps, 1 + 10);
    }

    #[test]
    fn changed_pane_before_paste_deletes_the_buffer() {
        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        let error = submit(&mut fake, "do the work", Proof::Turn(TURN_ID), 0)
            .0
            .unwrap_err();
        assert!(error.message.starts_with("pane changed before pasting"));
        assert!(!error.publication_uncertain);
        assert!(fake.sent("paste-buffer").is_empty());
        let name = &fake.sent("set-buffer")[0][2];
        assert_eq!(
            fake.sent("delete-buffer"),
            [&vec!["delete-buffer".to_owned(), "-b".into(), name.clone()]]
        );
        assert_eq!(fake.enters(), 0);
    }

    #[test]
    fn changed_pane_after_paste_is_never_submitted() {
        let mut fake = FakePane::new(&[CODEX_DRAFT]);
        let (result, _) = submit(&mut fake, "do the work", Proof::Turn(TURN_ID), 1);
        assert!(
            result
                .unwrap_err()
                .message
                .starts_with("pane changed after pasting; the task draft may remain unsubmitted")
        );
        assert_eq!(fake.enters(), 0);
    }

    #[test]
    fn task_buffers_are_unique_and_keep_a_terminal_semicolon_literal() {
        let mut fake = FakePane::new(&["› same;\n", "› same;\n", "› same;\n", CODEX_EMPTY]);
        submit(&mut fake, "same;", Proof::Turn(TURN_ID), 2)
            .0
            .unwrap();
        fake.reads = 0;
        submit(&mut fake, "same;", Proof::Turn(TURN_ID), 2)
            .0
            .unwrap();
        let sets = fake.sent("set-buffer");
        assert_ne!(sets[0][2], sets[1][2]);
        assert!(sets.iter().all(|c| c[4] == r"same\;"));
    }

    #[test]
    fn codex_and_agy_start_without_a_submitted_prompt() {
        for provider in ["codex", "antigravity"] {
            let command = orchestrator_command(
                provider,
                LeadLaunch::default().model(provider).unwrap(),
                None,
                super::MainPermissions::Full,
            )
            .unwrap();
            assert!(!command.iter().any(|arg| arg == STARTUP_PROMPT));
            assert!(!command.iter().any(|arg| arg == "--prompt-interactive"));
        }
    }

    #[test]
    fn provider_commands_apply_model_permissions_and_startup_role() {
        use super::MainPermissions;

        for model in ["gemini-4-neon-xhigh", "gemini-4-hydrogen-xhigh"] {
            assert!(
                orchestrator_command("antigravity", model, Some("xhigh"), MainPermissions::Native)
                    .is_ok()
            );
            assert!(
                orchestrator_command("antigravity", model, Some("max"), MainPermissions::Native)
                    .is_err()
            );
        }
        assert!(
            orchestrator_command("claude", "opus", Some("ultra"), MainPermissions::Full).is_err()
        );
        for (provider, program, model, permission_flag, prompt_option) in [
            (
                "codex",
                "codex",
                "gpt-6.2-sol",
                "--dangerously-bypass-approvals-and-sandbox",
                "--",
            ),
            (
                "claude",
                "claude",
                "opus",
                "--dangerously-skip-permissions",
                "--append-system-prompt",
            ),
            (
                "antigravity",
                "agy",
                "gemini-pro-orchestrator",
                "--dangerously-skip-permissions",
                "--prompt-interactive",
            ),
        ] {
            let mut expected = vec![program, "--model", model, permission_flag];
            if provider == "claude" {
                expected.extend([prompt_option, STARTUP_PROMPT]);
            }
            assert_eq!(
                orchestrator_command(
                    provider,
                    LeadLaunch::default().model(provider).unwrap(),
                    None,
                    MainPermissions::Full,
                )
                .unwrap(),
                expected,
            );
            expected.remove(3);
            assert_eq!(
                orchestrator_command(provider, model, None, MainPermissions::Native).unwrap(),
                expected,
            );
            for (request, effort) in [
                (Some("fixture/model-x"), None),
                (Some("fixture/model-x"), Some("xhigh")),
                (None, Some("high")),
            ] {
                let launch = LeadLaunch {
                    model: request,
                    effort,
                    ..Default::default()
                };
                let chosen = launch.model(provider).unwrap();
                let command = orchestrator_command(provider, chosen, effort, MainPermissions::Full);
                if provider == "antigravity" && request.is_none() {
                    assert!(command.is_err());
                    continue;
                }
                let mut expected = vec![
                    program,
                    "--model",
                    request.unwrap_or(model),
                    permission_flag,
                ];
                let config;
                if let Some(effort) = effort {
                    if provider == "codex" {
                        config = format!("model_reasoning_effort=\"{effort}\"");
                        expected.extend(["-c", config.as_str()]);
                    } else {
                        expected.extend(["--effort", effort]);
                    }
                }
                if provider == "claude" {
                    expected.extend([prompt_option, STARTUP_PROMPT]);
                }
                assert_eq!(command.unwrap(), expected);
                expected.remove(3);
                assert_eq!(
                    orchestrator_command(provider, chosen, effort, MainPermissions::Native)
                        .unwrap(),
                    expected,
                );
            }
        }
        for (request, canonical) in [
            ("Orchestrator (Carbon)", "gemini-pro-orchestrator"),
            ("Orchestrator (Argon)", "gemini-pro-orchestrator-argon"),
            ("gemini-pro-orchestrator", "gemini-pro-orchestrator"),
            (
                "GEMINI-PRO-ORCHESTRATOR-ARGON",
                "gemini-pro-orchestrator-argon",
            ),
        ] {
            let options = super::Options::parse(&super::commands::strings(&[
                "--orchestrator",
                "antigravity",
                "--model",
                request,
                "--reasoning-effort",
                "high",
            ]))
            .unwrap();
            let launch = LeadLaunch {
                model: options.get("model"),
                effort: options.get("reasoning-effort"),
                ..Default::default()
            };
            let selected = launch.model("antigravity").unwrap();
            assert_eq!(selected, canonical);
            assert!(
                orchestrator_command(
                    "antigravity",
                    selected,
                    launch.effort,
                    MainPermissions::Full
                )
                .is_err()
            );
            assert_eq!(
                orchestrator_command("antigravity", selected, None, MainPermissions::Full).unwrap(),
                [
                    "agy",
                    "--model",
                    canonical,
                    "--dangerously-skip-permissions"
                ]
            );
        }
    }

    #[test]
    fn native_child_model_cost_prompt_contract_preserves_codex_capacity() {
        let forbidden_aliases = [
            "haiku", "sonnet", "opus", "flash", "mini", "gpt-", "gemini-", "claude-", "o3", "o4",
        ];
        for (name, prompt) in [
            ("STARTUP_PROMPT", STARTUP_PROMPT.trim()),
            ("ALTERNATE_STARTUP_PROMPT", super::ALTERNATE_STARTUP_PROMPT),
        ] {
            let lower = prompt.to_ascii_lowercase();
            for required in [
                "cheaper supported native model",
                "simple bounded",
                "escalate",
                "scope",
                "risk",
                "user",
                "choice",
                "account",
                "isolation",
                "spend",
                "report",
                "actual",
                "model",
                "unknown",
            ] {
                assert!(
                    lower.contains(required),
                    "{name} missing required model-cost policy phrase {required:?}: {prompt}"
                );
            }
            for alias in forbidden_aliases {
                assert!(
                    !lower.contains(alias),
                    "{name} must not hardcode unauthenticated model alias {alias:?}: {prompt}"
                );
            }
        }

        assert!(
            super::ALTERNATE_STARTUP_PROMPT.chars().count() <= 517,
            "ALTERNATE_STARTUP_PROMPT grew past 517 chars ({})",
            super::ALTERNATE_STARTUP_PROMPT.chars().count(),
        );

        let locator = super::assignment_locator(
            std::path::Path::new("/repo/oh-my-setting"),
            "room-20261123-demo",
        )
        .unwrap();
        let task = "Implement the bounded change and run targeted unit checks.";
        let alternate_prompt = format!(
            "Task from Overall Main. Work within this task and report the result here. Turn {TURN_ID}. {}{locator} Task: {task}",
            super::ALTERNATE_STARTUP_PROMPT,
        );
        let primary_prompt = format!(
            "Task from Overall Main. Work within this task and report the result here. Turn {TURN_ID}. You own this provider's work. {}{locator} Task: {task}",
            super::ALTERNATE_STARTUP_PROMPT,
        );
        assert!(alternate_prompt.chars().count() <= super::CODEX_VISIBLE_PASTE_CHARS);
        assert!(primary_prompt.chars().count() <= super::CODEX_VISIBLE_PASTE_CHARS);
    }

    #[test]
    fn previous_main_live_provider_prevents_duplicate_native_pane() {
        let mut pane = [
            "$1",
            "@3",
            "%4",
            "123",
            "0",
            "0",
            "",
            "",
            "",
            "",
            "previous-room",
            "codex",
            "previous-main",
            "0",
            ".",
        ];
        assert_eq!(live_provider(&pane, "$1", "%1"), Some("codex"));
        assert_eq!(live_provider(&pane, "$2", "%1"), None);
        assert_eq!(live_provider(&pane, "$1", "%4"), None);
        pane[11] = "";
        assert_eq!(live_provider(&pane, "$1", "%1"), None);
        pane[11] = "codex";
        pane[4] = "1";
        assert_eq!(live_provider(&pane, "$1", "%1"), None);
    }

    #[test]
    fn completed_codex_goal_footer_is_observation_only() {
        let status = "  GPT-6.2-Sol high · Context 45% left · weekly 4% left · main · ~/fixture · Main [default]";
        let keys = "  ← for agents · ? for shortcuts";
        let prompt = "› \x1b[2mAsk Codex to do anything\x1b[0m";
        for suffix in [
            "Goal achieved (1d 0h 0m) · ⚠ 2 warnings · f2 to view",
            "Goal achieved (2d 23h 59m)",
            "Goal achieved (0s)",
            "Goal achieved (59m) · ⚠ 1 warning · f2 to view",
            "Goal achieved (1h)",
            "Goal achieved (23h 59m)",
        ] {
            let screen = format!("{prompt}\n{status}\n{keys}    {suffix}");
            assert_eq!(
                codex_model_from_chrome(&screen).as_deref(),
                Some("GPT-6.2-Sol"),
                "{suffix}"
            );
            assert!(!input_ready("codex", &screen));
            let mut alternate = super::CodexComposer {
                screen,
                cursor_x: 2,
                cursor_y: 0,
                command: "codex".into(),
            };
            assert!(!alternate.ready());
            alternate.screen = format!("› payload\n{status}\n{keys}    {suffix}");
            alternate.cursor_x = 9;
            assert!(!alternate.exact_draft("payload"));
        }
        for suffix in [
            "Goal achieved (60s)",
            "Goal achieved (60m)",
            "Goal achieved (24h)",
            "Goal achieved (0d 0h 0m)",
            "Goal achieved (10000d 0h 0m)",
            "Goal achieved (1d 24h 0m)",
            "Goal achieved (1d 0h 60m)",
            "Goal achieved (1d 0h 0m) · unknown text",
            "Goal achieved (1d 0h 0m) · ⚠ unknown warnings · f2 to view",
            "Goal achieved (1d 0h 0m)\x1b[2J",
            "Goal achieved (1d 0h 0m)\nmore text",
        ] {
            let screen = format!("{prompt}\n{status}\n{keys}    {suffix}");
            assert!(codex_model_from_chrome(&screen).is_none(), "{suffix:?}");
            assert!(!input_ready("codex", &screen));
        }
    }

    #[test]
    fn codex_status_footer_is_outside_the_draft_check() {
        let status = "\n\x1b[49m  \x1b[38;5;223mGPT-6.2-Sol high\x1b[0m · Context 100% left · weekly 48% left\n← for agents · ? for shortcuts";
        let prompt =
            "\x1b[1m›\x1b[0m\x1b[48;5;235m \x1b[2mAsk Codex to do anything\x1b[0m\x1b[48;5;235m\n";
        assert!(input_ready("codex", &format!("{prompt}{status}")));
        let display = format!("{prompt}{CODEX_DISPLAY_STATUS}");
        assert!(input_ready("codex", &display));
        assert_eq!(
            codex_model_from_chrome(&display).as_deref(),
            Some("GPT-6.2-Sol")
        );
        let no_profile = display.replace(" · Main [default]", "");
        let goal = "   Pursuing goal (20h 31m)";
        for suffix in [
            goal,
            "   Pursuing goal (20h 31m) · ⚠ 2 warnings · f2 to view",
        ] {
            let current = format!("{no_profile}{suffix}");
            assert_eq!(
                codex_model_from_chrome(&current).as_deref(),
                Some("GPT-6.2-Sol")
            );
            assert!(input_ready("codex", &current));
            let draft = current.replace(prompt, "› multiline draft\nsecond line\n");
            assert!(!input_ready("codex", &draft));
        }
        for fake in [
            " Pursuing goal (20h 60m)",
            " Pursuing goal (20h 31m) · arbitrary output",
            " Pursuing goal (20h 31m) · ⚠ unknown warnings · f2 to view",
            " arbitrary Pursuing goal (20h 31m)",
        ] {
            let screen = format!("{no_profile}{fake}");
            assert!(codex_model_from_chrome(&screen).is_none(), "{fake}");
            assert!(!input_ready("codex", &screen), "{fake}");
        }
        let custom_display = display
            .replace("GPT-6.2-Sol", "\x1b[36mfixture/model:v2\x1b[0m")
            .replace(" · weekly 13% left", "")
            .replace("~/fixture", "/fixture/work")
            .replace("Main [default]", "Main [seat_b]");
        assert!(input_ready("codex", &custom_display));
        assert_eq!(
            codex_model_from_chrome(&custom_display).as_deref(),
            Some("fixture/model:v2")
        );
        assert!(input_ready("codex", &format!("{prompt}────\n")));
        let warning = "   ⚠ 1 warning · f2 to view";
        assert!(input_ready("codex", &format!("{prompt}{status}{warning}")));
        assert!(!input_ready(
            "codex",
            &format!("› draft\n{status}{warning}")
        ));
        assert!(!input_ready("codex", &format!("› draft\n{status}")));
        assert!(!input_ready(
            "codex",
            &format!("{prompt}draft continued\n{status}")
        ));
        let custom = status.replace("GPT-6.2-Sol", "fixture/deepseek-v3");
        assert!(input_ready("codex", &format!("{prompt}{custom}")));
        assert!(!input_ready("codex", &format!("› draft\n{custom}")));
        assert!(!input_ready(
            "codex",
            &format!("{prompt}draft continued\n{custom}")
        ));
        let branch = custom.replace("48% left", "48% left · feature/work");
        assert!(input_ready("codex", &format!("{prompt}{branch}{warning}")));
        let alternate = |screen, cursor_x| super::CodexComposer {
            screen,
            cursor_x,
            cursor_y: 0,
            command: "codex".into(),
        };
        assert!(alternate(format!("{prompt}{branch}"), 2).ready());
        assert!(alternate(display.clone(), 2).ready());
        assert!(!alternate(display.clone(), 3).ready());
        let draft = alternate(format!("› payload\n{CODEX_DISPLAY_STATUS}"), 9);
        assert!(draft.exact_draft("payload"));
        assert!(!draft.exact_draft("payload hidden"));
        assert!(!draft.ready());
        for false_footer in [
            custom.replace("100% left", "about 80% left"),
            custom.replace("100% left", "101% left"),
            custom.replace("weekly 48% left", "weekly capacity unknown"),
            custom.replace("fixture/deepseek-v3", "arbitrary model prose"),
            custom.replace("← for agents · ? for shortcuts", "unrelated key legend"),
            format!("{custom}\nnew text below footer"),
        ] {
            assert!(!input_ready("codex", &format!("{prompt}{false_footer}")));
        }
        let status_only = CODEX_DISPLAY_STATUS.trim().lines().next().unwrap();
        assert_eq!(observed_model("codex", status_only), Some("GPT-6.2-Sol"));
        assert!(codex_model_from_chrome(status_only).is_none());
        for malformed in [
            CODEX_DISPLAY_STATUS.replace("~/fixture", "~/../fixture"),
            CODEX_DISPLAY_STATUS.replace("~/fixture", "arbitrary trailing prose"),
            CODEX_DISPLAY_STATUS.replace("Main [default]", "Main [def"),
            CODEX_DISPLAY_STATUS.replace("Main [default]", "Main [default] trailing"),
            CODEX_DISPLAY_STATUS.replace("← for agents", "← unrelated"),
            format!("{CODEX_DISPLAY_STATUS}\nmore text"),
        ] {
            let capture = format!("{prompt}────\n{malformed}");
            // The generic empty-input check stops at the lower rule.
            assert!(oms_panel_core::tick::screen_input_empty(&capture).0);
            assert!(super::codex_input_state(&capture).is_none());
            assert!(codex_model_from_chrome(&capture).is_none());
            assert!(observed_model("codex", &capture).is_none());
            assert!(!alternate(capture, 2).ready());
        }
        assert!(!input_ready(
            "codex",
            &format!("{prompt}────\ntruncated draft{CODEX_DISPLAY_STATUS}")
        ));
        assert!(!input_ready("codex", &format!("{prompt}{status_only}")));
        assert!(super::codex_input_state("› earlier\n│ › \n").is_none());
    }

    #[test]
    fn ordinary_codex_verifies_the_status_only_editor_and_rechecks_before_enter() {
        let rows = [
            "Task from Overall Main. Review the bounded work and describe the results. ",
            "Keep the checkout read only and report the evidence for each completion check. ",
            "Coordinate file ownership with the existing builder before considering changes. ",
            "Use the branch and task references supplied by the managed panel. ",
            "Inspect only the relevant callers and avoid inspecting other providers. ",
            "Return concrete observations and any uncertainty to the current Main. ",
            "Include the scoped verification checks.",
        ];
        let intended = rows.concat();
        let screen = |draft: &[&str], blank: &str, footer: &str| {
            format!(
                "{}› {}\n  {}\n{blank}\n{footer}",
                "\n".repeat(21),
                draft[0],
                draft[1..].join("\n  "),
            )
        };
        let status = CODEX_DISPLAY_STATUS
            .trim_start_matches('\n')
            .lines()
            .next()
            .unwrap();
        let exact = screen(&rows, "", status);
        let mut fake = FakePane::new(&[]);
        fake.draft_width = 145;
        fake.screens = vec![
            exact.clone(),
            exact.clone(),
            exact.clone(),
            format!("{CODEX_EMPTY}{CODEX_STATUS}"),
        ];
        submit(&mut fake, &intended, Proof::Turn(TURN_ID), 2)
            .0
            .unwrap();
        assert_eq!(fake.enters(), 1);
        assert_eq!(fake.sent("paste-buffer").len(), 1);

        let wrong = exact.replacen("read only", "read once", 1);
        let missing = exact.replacen("read only", "read", 1);
        let extra = exact.replacen("read only", "read only here", 1);
        let inverted = exact.replacen("read only", "\x1b[7mread only\x1b[0m", 1);
        let omitted_separator = exact.replacen("results. \n", "results.\n", 1);
        let malformed = exact.replace("Main [default]", "Main [default");
        let crowded = screen(&rows, "  unexpected row", status);
        let duplicate = screen(&rows, status, status);
        let missing_footer = screen(&rows, "", "");
        for other in [
            wrong.clone(),
            missing,
            extra,
            inverted,
            omitted_separator,
            malformed,
            crowded,
            duplicate,
            missing_footer,
        ] {
            fake.reads = 0;
            fake.calls.clear();
            fake.screens = vec![other];
            let error = submit(&mut fake, &intended, Proof::Turn(TURN_ID), 2)
                .0
                .unwrap_err();
            assert!(error.publication_uncertain);
            assert_eq!(fake.enters(), 0);
        }

        fake.reads = 0;
        fake.calls.clear();
        fake.draft_cursor_offset = 1;
        fake.screens = vec![exact.clone()];
        assert!(
            submit(&mut fake, &intended, Proof::Turn(TURN_ID), 2)
                .0
                .is_err()
        );
        assert_eq!(fake.enters(), 0);

        fake.reads = 0;
        fake.calls.clear();
        fake.draft_cursor_offset = 0;
        fake.screens = vec![exact.clone(), exact.clone(), wrong];
        let error = submit(&mut fake, &intended, Proof::Turn(TURN_ID), 2)
            .0
            .unwrap_err();
        assert!(error.message.contains("exact Codex draft changed"));
        assert_eq!(fake.enters(), 0);

        fake.reads = 0;
        fake.calls.clear();
        fake.screens = vec![exact.clone()];
        let error = submit(&mut fake, &intended, Proof::Turn(TURN_ID), 1)
            .0
            .unwrap_err();
        assert!(error.message.starts_with("pane changed after pasting"));
        assert_eq!(fake.enters(), 0);

        let frame = ordinary_frame("%7", &exact, 145, 0);
        for invalid in [
            frame.replacen("%7\t42", "%8\t42", 1),
            frame.replacen("42\t0\t0", "42\t0\t1", 1),
            frame.replacen("0\t0\t1\t0", "0\t0\t1\t1", 1),
        ] {
            assert!(
                super::ordinary_codex_draft(&mut |_| Ok(invalid.clone()), "%7", &intended)
                    .is_none()
            );
        }
        let idle = format!("{CODEX_EMPTY}\n{status}");
        assert!(!input_ready("codex", &idle));
        let spoof = ordinary_frame("%7", &idle, 145, 0);
        assert!(
            super::ordinary_codex_draft(
                &mut |_| Ok(spoof.clone()),
                "%7",
                "Ask Codex to do anything"
            )
            .is_none()
        );
    }

    #[test]
    fn malformed_codex_chrome_does_not_prove_a_pasted_draft() {
        let mut fake = FakePane::new(&[CODEX_EMPTY]);
        let malformed = CODEX_DISPLAY_STATUS.replace("Main [default]", "Main [default");
        fake.screens = vec![format!("{CODEX_EMPTY}────\n{malformed}")];
        let (result, _) = task(&mut fake);
        let err = result.unwrap_err();
        assert!(err.publication_uncertain && err.message.contains("paste was not observed"));
        assert_eq!(fake.sent("paste-buffer").len(), 1);
        assert_eq!(fake.enters(), 0);
    }

    #[test]
    fn terminal_semicolon_is_preserved_as_literal_input() {
        assert_eq!(literal_tmux_arg("before; after;"), r"before; after\;");
        assert_eq!(literal_tmux_arg(r"before\;"), r"before\\;");
        assert_eq!(literal_tmux_arg("before; after"), "before; after");
    }

    // Synthetic fixture for the preexisting bracketed block-bar shape.
    const AGY_QUOTA_DIALOG: &str = "└ Models & Quota\n\nGEMINI MODELS\n  Weekly Limit Remaining\n   [███] 94.62%\n  Five Hour Limit Remaining\n   [███] 86.37%\nCLAUDE AND GPT MODELS\n  Weekly Limit Remaining\n   [███] 100.00%";
    const AGY_QUOTA_MISORDERED: &str = "GEMINI MODELS\n  Weekly Limit Remaining\n   [███] 37%\n└ Models & Quota\n\nCLAUDE AND GPT MODELS\n  Weekly Limit Remaining\n   [███] 96%";
    // Screenshot-shaped viewport with unstyled spaces as captured without -e.
    const AGY_QUOTA_VIEWPORT: &str = concat!(
        ">\n────────────────────────────────────────\n└ Models & Quota\n\n  Account:\n\n",
        "GEMINI MODELS\n  Models within this group: Gemini Flash, Gemini Pro\n\n",
        "  Weekly Limit Remaining\n    [                                        ] 100.00%\n",
        "    Quota available\n\n",
        "  Five Hour Limit Remaining\n    [                                        ] 86.37%\n",
        "    Quota available\n\n",
        "CLAUDE AND GPT MODELS\n  Models within this group: Claude Opus, Claude Sonnet, GPT-OSS\n\n",
        "  Weekly Limit Remaining\n    [                                        ] 37.00%\n",
        "  (1–17 of 30 lines)\n\n",
        "  ↑/↓ Scroll · pgup/pgdown Page · ctrl+end Bottom · ctrl+home Top · esc Close\n",
        "                                                    Gemini 4 Argon · medium"
    );

    #[test]
    fn agy_quota_accepts_only_the_proved_headed_viewport() {
        let pane = AGY_QUOTA_VIEWPORT;
        let read = |screen: &str| weekly_from_usage("antigravity", screen);
        assert_eq!(read(pane), Some(0.0));
        assert_eq!(read(&pane.replacen("100.00%", "0.00%", 1)), Some(100.0));
        assert_eq!(read(pane.rsplit_once('\n').unwrap().0), Some(0.0));
        let alternate = pane
            .replace("1–17 of 30", "1–17 of 32")
            .replace("Account:", "Account: fixture@example.test")
            .replace(
                "Gemini Flash, Gemini Pro",
                "Fixture Flash Beta, Fixture Advanced/preview",
            )
            .replace(
                "Gemini 4 Argon · medium",
                "Fixture Model/preview (2) · high",
            );
        assert_eq!(read(&alternate), Some(0.0));
        for unproved in [
            format!("{pane}\n> new input"),
            format!("{pane}\n  Another Model · low"),
            format!("{pane}\n{pane}"),
            pane.replace("  Account:\n", ""),
            pane.replace("  Account:", "Account:"),
            pane.replace("Gemini Flash, Gemini Pro", ""),
            pane.replace("Claude Opus, Claude Sonnet, GPT-OSS", "> prompt"),
            pane.replacen("Weekly Limit Remaining", "Five Hour Limit Remaining", 1),
            pane.replacen("[                                        ] 100.00%", "", 1),
            pane.replacen(
                "[                                        ] 100.00%",
                "Footer 66%",
                1,
            ),
            pane.replacen("[                                        ]", "[███]", 1),
            pane.replacen("Quota available", "Unproved message", 1),
            pane.replace("1–17 of 30", "2–17 of 30"),
            pane.replace("1–17 of 30", "1–30 of 17"),
            pane.split_once("↑/↓ Scroll").unwrap().0.to_owned(),
            pane.replace("esc Close", "esc Continue"),
            pane.replace("Gemini 4 Argon · medium", ">"),
            pane.replace("Gemini 4 Argon · medium", "> Gemini 4 Argon · medium"),
            pane.replace("Gemini 4 Argon · medium", "Gemini 4 Argon · ultra"),
        ] {
            assert_eq!(read(&unproved), None, "{unproved}");
        }
    }

    #[test]
    fn weekly_quota_uses_the_correct_group_and_direction() {
        let claude = "  Settings   Status   Config   Usage   Stats\n\n  Current week (all models)\n  ███      80% used\n\n  Current week (Fable)\n  ███      82% used";
        let agy = AGY_QUOTA_DIALOG;
        assert_eq!(weekly_from_usage("claude", claude), Some(80.0));
        assert!((weekly_from_usage("antigravity", agy).unwrap() - 5.38).abs() < 0.001);
        assert_eq!(
            weekly_from_usage("antigravity", &agy.replace("GEMINI MODELS", "")),
            None
        );
        assert_eq!(weekly_from_usage("claude", "7d 79%"), None);
        assert_eq!(weekly_from_usage("codex", agy), None);
        assert_eq!(weekly_from_usage("antigravity", AGY_QUOTA_MISORDERED), None);
        for screen in [
            format!(
                "{agy}\n> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)\n────"
            ),
            format!("{agy}\n{agy}"),
            agy.replace("└ Models & Quota", "Models & Quota"),
            agy.replace("[███] 94.62%", "Footer says 94.62%"),
            agy.replace("94.62%", "9.462e1%"),
            agy.replace("94.62%", "100.01%"),
            agy.replace("94.62%", "100.00000000000000000001%"),
            agy.replace(
                "  Weekly Limit Remaining\n   [███] 94.62%",
                "  Five Hour Limit Remaining\n   [███] 94.62%",
            ),
            agy.split_once("CLAUDE AND GPT MODELS")
                .unwrap()
                .0
                .to_owned(),
            agy.strip_suffix("100.00%").unwrap().to_owned(),
        ] {
            assert_eq!(weekly_from_usage("antigravity", &screen), None, "{screen}");
        }
        for (remaining, used) in [("0%", 100.0), ("100%", 0.0)] {
            let screen = agy.replace("94.62%", remaining);
            assert_eq!(weekly_from_usage("antigravity", &screen), Some(used));
        }
    }

    #[test]
    fn weekly_merge_keeps_one_newest_provenance() {
        use super::merge_weekly;
        use crate::claude_lead_usage::LeadReading;
        use serde_json::json;

        let lead = |at: f64, used: Option<f64>| LeadReading {
            weekly_used: used,
            resets_at: Some(9000.0),
            observed_at: at,
            stale: used.is_none(),
        };
        let merged = |row: serde_json::Value, dialog, reading: Option<LeadReading>| {
            let mut row = row;
            merge_weekly(&mut row, dialog, reading.as_ref());
            row
        };
        let base = json!({"source": "unavailable"});
        // Dialog alone keeps its existing provenance; a newer or tied dialog wins.
        let dialog = json!({"source": "unavailable", "weekly_used": 80.0, "weekly_native_observed_at": 100.0});
        assert_eq!(merged(base.clone(), Some((80.0, 100.0)), None), dialog);
        assert_eq!(
            merged(
                base.clone(),
                Some((80.0, 100.0)),
                Some(lead(100.0, Some(7.0)))
            ),
            dialog
        );
        // A newer lead reading replaces an older sticky dialog sample.
        assert_eq!(
            merged(
                base.clone(),
                Some((80.0, 100.0)),
                Some(lead(150.0, Some(7.0)))
            ),
            json!({"source": "unavailable", "weekly_used": 7.0, "weekly_lead_observed_at": 150.0, "weekly_resets_at": 9000.0})
        );
        // A row observed after the lead reading is left untouched.
        let fresher = json!({"source": "HUD cache", "observed_at": 200.0, "weekly_used": 30});
        assert_eq!(
            merged(fresher.clone(), None, Some(lead(150.0, Some(7.0)))),
            fresher
        );
        assert_eq!(
            merged(fresher.clone(), None, Some(lead(150.0, None))),
            fresher
        );
        // A stale lead reading marks the row stale and drops every number.
        assert_eq!(
            merged(dialog.clone(), Some((80.0, 100.0)), Some(lead(150.0, None))),
            json!({"source": "cache stale", "observed_at": 150.0})
        );
    }

    #[test]
    fn observes_models_from_the_native_status_rows() {
        for (provider, screen, model) in [
            (
                "codex",
                "  GPT-6.2-Sol high · Context 100% left · weekly 54% left",
                "GPT-6.2-Sol",
            ),
            (
                "claude",
                "  Sonnet 5.5 | branch | medium+think | ctx [----] 6%",
                "Sonnet 5.5",
            ),
            (
                "claude",
                "  Fable 5.2 | branch | xhigh+think | ctx [----] 6%",
                "Fable 5.2",
            ),
            (
                "claude",
                "  Fable 5.2 | git branch | ctx [##--------] 21% 214k/1m | 7d 54%",
                "Fable 5.2",
            ),
            ("claude", "  Fable 5.1 | ctx --", "Fable 5.1"),
            (
                "antigravity",
                "Gemini 4 Hydrogen (High) (Antigravity ) | ctx 2% | branch",
                "Gemini 4 Hydrogen (High)",
            ),
        ] {
            assert_eq!(observed_model(provider, screen), Some(model));
        }
        assert_eq!(observed_model("codex", "Probably GPT-6.2-Sol"), None);
        for false_footer in [
            "Probably Fable 5.2",
            "Fable research notes | ctx example",
            "Fable research notes | ctx [----] 6%",
            "Fable 5.2 | branch | ctx example",
            "Fable 5.2 | branch | ctx [----] 106%",
        ] {
            assert_eq!(
                observed_model("claude", false_footer),
                None,
                "{false_footer}"
            );
        }
        let mixed = "  Sonnet 5.5 | branch | ctx [----] 6%\nFable research notes | ctx example";
        assert_eq!(observed_model("claude", mixed), Some("Sonnet 5.5"));
    }

    #[test]
    fn native_placeholder_is_distinct_from_a_draft() {
        let agy = "\x1b[38;5;111m>\x1b[39m \x1b[38;5;109mAuto mode: edits & safe commands auto-approved (shift+tab to cycle)\x1b[39m\n────\n";
        assert!(input_ready("antigravity", agy));
        assert!(!input_ready(
            "antigravity",
            &agy.replace("\x1b[38;5;109m", "")
        ));
        assert!(input_ready(
            "claude",
            "❯\u{a0}\x1b[2mTry something\x1b[0m\n────\n"
        ));
        assert!(!input_ready("claude", "❯\u{a0}Work in progress\n────\n"));
    }

    mod account_tests {
        use super::super::*;
        use super::{
            AGY_QUOTA_MISORDERED, AGY_QUOTA_VIEWPORT, CODEX_DISPLAY_STATUS, CODEX_DRAFT,
            CODEX_EMPTY, CODEX_OPAQUE_PASTE, CODEX_STATUS, TURN_ID, ordinary_frame,
        };
        use crate::orchestrator::account_binding::test_support::{FakeHost, TempDir, private_dir};
        use oms_panel_core::native::reference::process::Owner;
        use oms_panel_core::services::ExecutionRequest;
        use oms_panel_core::{CommandOutput, PeerExecutor};
        use std::cell::{Cell, RefCell};
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;

        const ROOM: &str = "room-acct";
        const MAIN: &str = "att_11111111111111111111111111111111";

        include!("native_succession_tests.rs");

        struct FakeLead {
            window: String,
            pane_id: String,
            pid: u32,
            active: bool,
            provider: String,
        }

        type TaskTransition = (&'static str, Box<dyn FnOnce(&SpyPanel)>);

        struct SpyPanel {
            repo: PathBuf,
            room: String,
            room_closed: Cell<bool>,
            room_successor: Cell<Option<&'static str>>,
            room_query_fails: Cell<bool>,
            room_queries: RefCell<Vec<bool>>,
            event_projection: RefCell<Option<Value>>,
            real_events: Cell<bool>,
            canonical_assignment: Cell<bool>,
            // Canonical-query port for mutation and retirement boundary fixtures;
            // this never writes a Plan completion or a landing receipt.
            task_snapshot_override: RefCell<Option<Value>>,
            event_lists: Cell<usize>,
            assignment_events: Cell<usize>,
            main_attempt: String,
            main_participant: String,
            ordinary_parent: Option<String>,
            main_pane: String,
            main_pid: u32,
            main_owner: Cell<&'static str>,
            main_account_ready: Cell<bool>,
            codex_chat_marks: RefCell<BTreeMap<String, String>>,
            requested_model_marks: RefCell<BTreeMap<String, String>>,
            claude_quota_marks: RefCell<BTreeMap<String, String>>,
            alternate_parents: RefCell<BTreeMap<String, String>>,
            alternate_locally_marked: Cell<bool>,
            alternate_spawn_reply: RefCell<Option<CommandOutput>>,
            alternate_task_mode: Cell<bool>,
            alternate_task_command: Cell<&'static str>,
            alternate_task_cursor: Cell<(usize, usize)>,
            dead_panes: RefCell<BTreeSet<String>>,
            server_pid: u32,
            socket_path: String,
            session_reply: RefCell<Option<CommandOutput>>,
            stop: AtomicBool,
            leads: RefCell<Vec<FakeLead>>,
            global_env: RefCell<Vec<String>>,
            session_env: RefCell<Vec<String>>,
            screens: RefCell<BTreeMap<String, String>>,
            recent_screens: RefCell<BTreeMap<String, String>>,
            next_recent: RefCell<Option<String>>,
            calls: RefCell<Vec<Vec<String>>>,
            next_pane: Cell<u32>,
            next_window: Cell<u32>,
            spawn_pid: Cell<u32>,
            fail_parent_tag: Cell<bool>,
            provider_tag_writes: RefCell<Vec<String>>,
            cursor_x: Cell<u32>,
            allow_board_queries: Cell<bool>,
            dock_geometries: RefCell<BTreeMap<String, String>>,
            dock_after_break: RefCell<Option<[String; 2]>>,
            dock_rows: RefCell<String>,
            task_transition: RefCell<Option<TaskTransition>>,
            primary_columns: Cell<Option<usize>>,
        }

        impl SpyPanel {
            fn new(repo: PathBuf) -> Self {
                let pid = std::process::id();
                Self {
                    repo,
                    room: ROOM.into(),
                    room_closed: Cell::new(false),
                    room_successor: Cell::new(None),
                    room_query_fails: Cell::new(false),
                    room_queries: RefCell::new(Vec::new()),
                    event_projection: RefCell::new(None),
                    real_events: Cell::new(false),
                    canonical_assignment: Cell::new(false),
                    task_snapshot_override: RefCell::new(None),
                    event_lists: Cell::new(0),
                    assignment_events: Cell::new(0),
                    main_attempt: MAIN.into(),
                    main_participant: MAIN.into(),
                    ordinary_parent: None,
                    main_pane: "%1".into(),
                    main_pid: pid,
                    main_owner: Cell::new("codex"),
                    main_account_ready: Cell::new(false),
                    codex_chat_marks: RefCell::new(BTreeMap::new()),
                    requested_model_marks: RefCell::new(BTreeMap::new()),
                    claude_quota_marks: RefCell::new(BTreeMap::new()),
                    alternate_parents: RefCell::new(BTreeMap::new()),
                    alternate_locally_marked: Cell::new(true),
                    alternate_spawn_reply: RefCell::new(None),
                    alternate_task_mode: Cell::new(false),
                    alternate_task_command: Cell::new("codex"),
                    alternate_task_cursor: Cell::new((2, 0)),
                    dead_panes: RefCell::new(BTreeSet::new()),
                    server_pid: pid,
                    socket_path: "fixture-socket".into(),
                    session_reply: Default::default(),
                    stop: AtomicBool::new(false),
                    leads: RefCell::new(Vec::new()),
                    global_env: RefCell::new(vec!["OMS_AGENT=codex".into()]),
                    session_env: RefCell::new(Vec::new()),
                    screens: RefCell::new(BTreeMap::new()),
                    recent_screens: RefCell::new(BTreeMap::new()),
                    next_recent: RefCell::new(None),
                    calls: RefCell::new(Vec::new()),
                    next_pane: Cell::new(4),
                    next_window: Cell::new(4),
                    spawn_pid: Cell::new(2001),
                    fail_parent_tag: Cell::new(false),
                    provider_tag_writes: RefCell::new(Vec::new()),
                    cursor_x: Cell::new(2),
                    allow_board_queries: Cell::new(false),
                    dock_geometries: Default::default(),
                    dock_after_break: Default::default(),
                    dock_rows: Default::default(),
                    task_transition: RefCell::new(None),
                    primary_columns: Cell::new(None),
                }
            }

            fn load(&self) -> Panel<'_> {
                Panel::load_for_caller(&self.repo, &self.room, "%1", self, &self.stop).unwrap()
            }

            fn mutating_calls(&self) -> Vec<Vec<String>> {
                const MUTATORS: &[&str] = &[
                    "new-window",
                    "kill-pane",
                    "set-option",
                    "send-keys",
                    "paste-buffer",
                    "set-buffer",
                    "break-pane",
                    "join-pane",
                ];
                self.calls
                    .borrow()
                    .iter()
                    .filter(|args| args.get(1).is_some_and(|v| MUTATORS.contains(&v.as_str())))
                    .cloned()
                    .collect()
            }

            fn calls_named(&self, verb: &str) -> Vec<Vec<String>> {
                self.calls
                    .borrow()
                    .iter()
                    .filter(|args| args.get(1).is_some_and(|v| v == verb))
                    .cloned()
                    .collect()
            }
        }

        fn out(stdout: impl Into<Vec<u8>>) -> CommandOutput {
            CommandOutput {
                stdout: stdout.into(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            }
        }

        impl PeerExecutor for SpyPanel {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                if self.canonical_assignment.get()
                    && request.argv.first().is_some_and(|v| v == "git")
                {
                    let output = std::process::Command::new("git")
                        .args(&request.argv[1..])
                        .current_dir(&request.cwd)
                        .output()
                        .unwrap();
                    return Ok(CommandOutput {
                        code: output.status.code().unwrap_or(1),
                        stdout: output.stdout,
                        stderr: output.stderr,
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    });
                }
                let args: Vec<String> = request
                    .argv
                    .iter()
                    .map(|w| w.to_string_lossy().into_owned())
                    .collect();
                self.calls.borrow_mut().push(args.clone());
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let body = match refs.as_slice() {
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        _,
                        "#{pane_current_path}",
                    ] if self.canonical_assignment.get() => self.repo.to_str().unwrap().into(),
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        native_assignment::PRIMARY_TICKET_LAYOUT,
                    ] => {
                        let leads = self.leads.borrow();
                        let lead = leads.iter().find(|lead| lead.pane_id == *pane).unwrap();
                        let active = u8::from(lead.active);
                        let width = self.primary_columns.get().unwrap_or(80);
                        format!(
                            "$1\t{}\t{pane}\t{}\t0\t0\t1\t{active}\t{width}\t.",
                            lead.window, lead.pid,
                        )
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        lead_delivery::FRAME,
                        capture @ ..,
                    ] => {
                        let screens = self.screens.borrow();
                        let screen = &screens[*pane];
                        let row = screen
                            .lines()
                            .position(|line| CHAT_SGR.replace_all(line, "").starts_with(['❯', '>']))
                            .unwrap();
                        let body = if capture.contains(&"-J") {
                            screen.lines().nth(row).unwrap()
                        } else {
                            screen
                        };
                        super::lead_frame(pane, body, self.cursor_x.get() as usize, row, 8192)
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        ORDINARY_CODEX_DRAFT_FORMAT,
                        ..,
                    ] => {
                        let screens = self.screens.borrow();
                        ordinary_frame(
                            pane,
                            screens.get(*pane).map_or("", String::as_str),
                            self.primary_columns.get().unwrap_or(80),
                            0,
                        )
                    }
                    ["tmux", "list-panes", "-s", "-t", _, "-F", PANE_FORMAT] => {
                        let mut lines = vec![format!(
                            "$1\t@1\t{}\t{}\t0\t1\t{}\t{}\t{}\t{}\t{}\t\t\t1\t.{}__",
                            self.main_pane,
                            self.main_pid,
                            self.main_pane,
                            self.main_attempt,
                            self.main_owner.get(),
                            self.main_participant,
                            self.room,
                            if self.main_account_ready.get() {
                                "1"
                            } else {
                                ""
                            },
                        )];
                        for lead in self.leads.borrow().iter() {
                            let active = u8::from(lead.active);
                            let dead = u8::from(self.dead_panes.borrow().contains(&lead.pane_id));
                            let alternate = self.alternate_parents.borrow();
                            let (room, parent) = alternate.get(&lead.pane_id).map_or(
                                (
                                    self.room.as_str(),
                                    self.ordinary_parent
                                        .as_deref()
                                        .unwrap_or(&self.main_attempt),
                                ),
                                |parent| ("", parent.as_str()),
                            );
                            lines.push(format!(
                                "$1\t{}\t{}\t{}\t{dead}\t{active}\t\t\t\t\t{}\t{}\t{}\t{active}\t._{}_{}",
                                lead.window,
                                lead.pane_id,
                                lead.pid,
                                room,
                                lead.provider,
                                parent,
                                self.codex_chat_marks
                                    .borrow()
                                    .get(&lead.pane_id)
                                    .map_or("", String::as_str),
                                self.claude_quota_marks
                                    .borrow()
                                    .get(&lead.pane_id)
                                    .map_or("", String::as_str),
                            ));
                        }
                        lines.join("\n")
                    }
                    ["tmux", "display-message", "-p", "-t", "$1", _] => {
                        format!("$1\t{}\t{}", self.server_pid, self.socket_path)
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        "$1",
                        terminals::PROVIDER_SESSION_IDENTITY_FORMAT,
                        ";",
                        "show-options",
                        "-v",
                        "-t",
                        "$1",
                        "@oms_panel_repo",
                    ] => {
                        if let Some(reply) = self.session_reply.borrow_mut().take() {
                            return Ok(reply);
                        }
                        format!(
                            "$1\t{}\t{}\t.\n{}\n",
                            self.server_pid,
                            self.socket_path,
                            self.repo.display()
                        )
                    }
                    [
                        "tmux",
                        "list-panes",
                        "-t",
                        "%2",
                        "-F",
                        NATIVE_DOCK_GEOMETRY,
                        extra @ ..,
                    ] => {
                        let geometry = self.dock_geometries.borrow();
                        let mut rows: Vec<_> = geometry
                            .values()
                            .filter(|row| row.split('\t').nth(1) == Some("@1"))
                            .map(String::as_str)
                            .collect();
                        if let [
                            ";",
                            "display-message",
                            "-p",
                            "-t",
                            pane,
                            NATIVE_DOCK_GEOMETRY,
                        ] = extra
                        {
                            rows.push(geometry[*pane].as_str());
                        } else {
                            assert!(extra.is_empty(), "{extra:?}");
                        }
                        rows.join("\n")
                    }
                    ["tmux", "show-options", "-pqv", "-t", "%2", NATIVE_DOCK_ROWS] => {
                        self.dock_rows.borrow().clone()
                    }
                    [
                        "tmux",
                        "set-option",
                        "-p",
                        "-t",
                        "%2",
                        NATIVE_DOCK_ROWS,
                        rows,
                    ] => {
                        self.dock_rows.replace((*rows).into());
                        String::new()
                    }
                    [
                        "tmux",
                        "break-pane",
                        "-d",
                        "-s",
                        pane,
                        "-n",
                        _,
                        ";",
                        "set-option",
                        "-pqu",
                        "-t",
                        "%2",
                        NATIVE_DOCK_ROWS,
                    ] => {
                        self.dock_rows.replace(String::new());
                        let mut geometry = self.dock_geometries.borrow_mut();
                        geometry.remove(*pane);
                        if let Some([board, main]) = self.dock_after_break.borrow_mut().take() {
                            geometry.insert("%2".into(), board);
                            geometry.insert("%1".into(), main);
                        }
                        String::new()
                    }
                    [
                        "tmux",
                        "join-pane",
                        "-d",
                        "-v",
                        "-l",
                        _,
                        "-s",
                        _,
                        "-t",
                        "%2",
                        ";",
                        "set-option",
                        "-pqu",
                        "-t",
                        "%2",
                        NATIVE_DOCK_ROWS,
                    ] => {
                        self.dock_rows.replace(String::new());
                        String::new()
                    }
                    ["tmux", "display-message", "-p", "-t", "%2", _]
                        if self.allow_board_queries.get() =>
                    {
                        format!(
                            "$1\t@1\t{}\t0\t1\t0\t1\t{}\t{}\t{}",
                            self.main_pid,
                            self.main_attempt,
                            self.room,
                            self.repo.display()
                        )
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        _,
                        "#{pane_in_mode}|#{cursor_x}|#{cursor_y}",
                    ] => {
                        format!("0|{}|0", self.cursor_x.get())
                    }
                    ["tmux", "show-options", "-v", "-t", "$1", "@oms_panel_repo"] => {
                        self.repo.display().to_string()
                    }
                    ["tmux", "show-environment", "-g"] => self.global_env.borrow().join("\n"),
                    ["tmux", "show-environment", "-t", "$1"] => {
                        self.session_env.borrow().join("\n")
                    }
                    ["tmux", "new-window", ..] => {
                        let pane_idx = self.next_pane.get();
                        self.next_pane.set(pane_idx + 1);
                        let win_idx = self.next_window.get();
                        self.next_window.set(win_idx + 1);
                        let pane_id = format!("%{pane_idx}");
                        let window = format!("@{win_idx}");
                        let pid = self.spawn_pid.get();
                        let provider = if args.iter().any(|a| a == "Codex Alternate") {
                            self.alternate_parents
                                .borrow_mut()
                                .insert(pane_id.clone(), String::new());
                            ""
                        } else if args.iter().any(|a| a == "claude") {
                            "claude"
                        } else if args.iter().any(|a| a == "agy") {
                            "antigravity"
                        } else {
                            "codex"
                        };
                        self.leads.borrow_mut().push(FakeLead {
                            window,
                            pane_id: pane_id.clone(),
                            pid,
                            active: false,
                            provider: provider.into(),
                        });
                        if provider.is_empty() {
                            if let Some(reply) = self.alternate_spawn_reply.borrow_mut().take() {
                                return Ok(reply);
                            }
                            format!("{pane_id}\t{pid}\t@{win_idx}\t$1")
                        } else {
                            format!("{pane_id}\t{pid}")
                        }
                    }
                    [
                        "tmux",
                        "set-option",
                        "-p",
                        "-t",
                        pane,
                        "@oms_provider_parent",
                        ALTERNATE_LEAD_TAG,
                        ";",
                        "set-option",
                        "-w",
                        "-t",
                        target,
                        "@oms_provider_session",
                        "codex",
                    ] => {
                        assert_eq!(pane, target);
                        let pane = pane.rsplit_once('.').map_or(*pane, |(_, pane)| pane);
                        if self.fail_parent_tag.get() {
                            return Ok(CommandOutput { code: 1, ..out("") });
                        }
                        *self.alternate_parents.borrow_mut().get_mut(pane).unwrap() =
                            ALTERNATE_LEAD_TAG.into();
                        self.leads
                            .borrow_mut()
                            .iter_mut()
                            .find(|l| l.pane_id == pane)
                            .unwrap()
                            .provider = "codex".into();
                        String::new()
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        ALTERNATE_TASK_INPUT_FORMAT,
                        ";",
                        "show-options",
                        "-pqv",
                        "-t",
                        option_pane,
                        "@oms_provider_parent",
                        ";",
                        "display-message",
                        "-p",
                        "-t",
                        frame_pane,
                        ".",
                        ";",
                        "capture-pane",
                        "-p",
                        "-e",
                        "-J",
                        "-S",
                        "0",
                        "-t",
                        capture_pane,
                    ] => {
                        assert_eq!([pane, pane, pane], [option_pane, frame_pane, capture_pane]);
                        let (window, pane) = pane.split_once('.').expect("qualified pane");
                        let leads = self.leads.borrow();
                        let Some(lead) = leads.iter().find(|lead| {
                            lead.pane_id == pane && window == format!("$1:{}", lead.window)
                        }) else {
                            return Ok(out(""));
                        };
                        let count = leads
                            .iter()
                            .filter(|entry| entry.window == lead.window)
                            .count();
                        let dead = u8::from(self.dead_panes.borrow().contains(pane));
                        let parents = self.alternate_parents.borrow();
                        let marker = parents
                            .get(pane)
                            .filter(|_| self.alternate_locally_marked.get());
                        let marker = marker.map(|tag| format!("{tag}\n")).unwrap_or_default();
                        let active = u8::from(lead.active);
                        let mode = u8::from(self.alternate_task_mode.get());
                        let (x, y) = self.alternate_task_cursor.get();
                        let command = self.alternate_task_command.get();
                        let screens = self.screens.borrow();
                        let screen = screens.get(pane).map_or("", String::as_str);
                        format!(
                            "$1\t{}\t{pane}\t{}\t{dead}\t{count}\t.\t{active}\t{mode}\t{x}\t{y}\t{command}\t.\n{marker}.\n{screen}",
                            lead.window, lead.pid,
                        )
                    }
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        ALTERNATE_PANE_FORMAT,
                        tail @ ..,
                    ] => {
                        let leads = self.leads.borrow();
                        let Some(lead) = leads.iter().find(|lead| lead.pane_id == *pane) else {
                            return Ok(out(""));
                        };
                        let count = leads
                            .iter()
                            .filter(|entry| entry.window == lead.window)
                            .count();
                        let dead = u8::from(self.dead_panes.borrow().contains(*pane));
                        let mut result = format!(
                            "$1\t{}\t{}\t{}\t{dead}\t{count}\t.",
                            lead.window, lead.pane_id, lead.pid
                        );
                        if !tail.is_empty() {
                            assert_eq!(
                                tail,
                                &[
                                    ";",
                                    "show-options",
                                    "-pqv",
                                    "-t",
                                    pane,
                                    "@oms_provider_parent"
                                ]
                            );
                            if self.alternate_locally_marked.get()
                                && let Some(marker) = self.alternate_parents.borrow().get(*pane)
                            {
                                result.push('\n');
                                result.push_str(marker);
                            }
                        }
                        result
                    }
                    [
                        "tmux",
                        "select-pane",
                        "-t",
                        _,
                        ";",
                        "select-window",
                        "-t",
                        _,
                    ]
                    | ["tmux", "select-window", "-t", _]
                    | ["tmux", "select-pane", "-t", _] => String::new(),
                    [
                        "tmux",
                        "set-option",
                        "-p",
                        "-t",
                        pane,
                        "@oms_codex_chat_bound",
                        value,
                    ] => {
                        self.codex_chat_marks
                            .borrow_mut()
                            .insert((*pane).into(), (*value).into());
                        String::new()
                    }
                    [
                        "tmux",
                        "set-option",
                        "-p",
                        "-t",
                        pane,
                        REQUESTED_PROVIDER_MODEL,
                        value,
                    ] => {
                        self.requested_model_marks
                            .borrow_mut()
                            .insert((*pane).into(), (*value).into());
                        String::new()
                    }
                    [
                        "tmux",
                        "show-options",
                        "-pqv",
                        "-t",
                        pane,
                        REQUESTED_PROVIDER_MODEL,
                    ] => self
                        .requested_model_marks
                        .borrow()
                        .get(*pane)
                        .cloned()
                        .unwrap_or_default(),
                    [
                        "tmux",
                        "set-option",
                        "-p",
                        "-t",
                        pane,
                        "@oms_claude_quota_bound",
                        value,
                    ] => {
                        self.claude_quota_marks
                            .borrow_mut()
                            .insert((*pane).into(), (*value).into());
                        String::new()
                    }
                    [
                        "tmux",
                        "set-option",
                        "-w" | "-p",
                        "-t",
                        _,
                        "@oms_panel_room",
                        ..,
                    ] => {
                        for tag in refs[1..].split(|arg| *arg == ";") {
                            let ["set-option", "-w" | "-p", "-t", _, key, _] = tag else {
                                panic!("invalid fixture provider tag {tag:?}");
                            };
                            if *key == "@oms_provider_parent" && self.fail_parent_tag.get() {
                                return Ok(CommandOutput { code: 1, ..out("") });
                            }
                            self.provider_tag_writes.borrow_mut().push((*key).into());
                        }
                        String::new()
                    }
                    ["tmux", "kill-pane", "-t", pane] => {
                        let pane = pane.rsplit_once('.').map_or(*pane, |(_, pane)| pane);
                        self.leads.borrow_mut().retain(|l| l.pane_id != pane);
                        String::new()
                    }
                    ["tmux", "rename-window", ..]
                    | ["tmux", "set-option", "-t", "$1", "mouse", "on"] => String::new(),
                    ["tmux", "set-buffer", "-b", _, "--", text] => {
                        *self
                            .recent_screens
                            .borrow_mut()
                            .entry("_pending_paste".into())
                            .or_default() = (*text).into();
                        String::new()
                    }
                    ["tmux", "delete-buffer", "-b", _] => String::new(),
                    ["tmux", "paste-buffer", "-p", "-d", "-b", _, "-t", pane] => {
                        let pane = pane.rsplit_once('.').map_or(*pane, |(_, pane)| pane);
                        if self.alternate_parents.borrow().contains_key(pane) {
                            let text = self.recent_screens.borrow()["_pending_paste"].clone();
                            let text = text
                                .strip_suffix(r"\;")
                                .map_or(Cow::Borrowed(text.as_str()), |p| format!("{p};").into());
                            let (draft, cursor) = alternate_draft(&text);
                            self.screens.borrow_mut().insert(pane.into(), draft);
                            self.alternate_task_cursor.set(cursor);
                        } else if self
                            .leads
                            .borrow()
                            .iter()
                            .any(|lead| lead.pane_id == pane && lead.provider == "codex")
                        {
                            let text = self.recent_screens.borrow()["_pending_paste"].clone();
                            let text = text
                                .strip_suffix(r"\;")
                                .map_or(Cow::Borrowed(text.as_str()), |p| format!("{p};").into());
                            let (draft, cursor) = match self.primary_columns.get() {
                                Some(width) if crate::render::width(&text) + 2 <= width => {
                                    let status = CODEX_DISPLAY_STATUS.lines().nth(1).unwrap();
                                    let draft = format!("{}› {text}\n\n{status}", "\n".repeat(87));
                                    (draft, (crate::render::width(&text) + 2, 87))
                                }
                                _ => alternate_draft(&text),
                            };
                            self.cursor_x.set(cursor.0 as u32);
                            self.screens.borrow_mut().insert(pane.into(), draft);
                        } else {
                            self.cursor_x.set(12);
                            self.screens
                                .borrow_mut()
                                .insert(pane.into(), format!("{CODEX_DRAFT}{CODEX_STATUS}"));
                        }
                        String::new()
                    }
                    ["tmux", "send-keys", "-t", pane, "Enter"] => {
                        let pane = pane.rsplit_once('.').map_or(*pane, |(_, pane)| pane);
                        self.cursor_x.set(2);
                        let pasted = self
                            .recent_screens
                            .borrow()
                            .get("_pending_paste")
                            .cloned()
                            .unwrap_or_default();
                        let screen = if self.alternate_parents.borrow().contains_key(pane) {
                            let (message, (_, last)) = alternate_draft(&pasted);
                            let message: Vec<_> = message.lines().take(last + 1).collect();
                            self.alternate_task_cursor.set((2, last + 2));
                            format!("{}\n\n{CODEX_EMPTY}{CODEX_STATUS}", message.join("\n"))
                        } else {
                            format!("{CODEX_EMPTY}{CODEX_STATUS}")
                        };
                        self.screens.borrow_mut().insert(pane.into(), screen);
                        self.recent_screens
                            .borrow_mut()
                            .insert(pane.into(), format!("› {pasted}\n\n› Ask Codex"));
                        String::new()
                    }
                    ["tmux", "send-keys", ..] => String::new(),
                    ["tmux", "capture-pane", "-p", "-t", pane, "-S", _, "-E", _]
                    | ["tmux", "capture-pane", "-p", "-S", "0", "-E", _, "-t", pane] => self
                        .screens
                        .borrow()
                        .get(*pane)
                        .and_then(|s| s.lines().next())
                        .map(|line| CHAT_SGR.replace_all(line, "").into_owned())
                        .unwrap_or_default(),
                    ["tmux", "capture-pane", "-p", "-t", pane]
                    | ["tmux", "capture-pane", "-p", "-e", "-S", _, "-t", pane] => self
                        .screens
                        .borrow()
                        .get(*pane)
                        .cloned()
                        .unwrap_or_default(),
                    ["tmux", "capture-pane", "-p", "-J", "-S", "-100", "-t", pane]
                    | [
                        "tmux",
                        "capture-pane",
                        "-p",
                        "-J",
                        "-S",
                        "-200",
                        "-E",
                        _,
                        "-t",
                        pane,
                    ] => {
                        let shown = self
                            .recent_screens
                            .borrow()
                            .get(*pane)
                            .cloned()
                            .unwrap_or_default();
                        if let Some(next) = self.next_recent.borrow_mut().take() {
                            self.recent_screens
                                .borrow_mut()
                                .insert((*pane).into(), next);
                        }
                        shown
                    }
                    _ => panic!("unexpected tmux call: {args:?}"),
                };
                let operation = if refs.get(5) == Some(&ALTERNATE_TASK_INPUT_FORMAT) {
                    Some("alternate-task-capture")
                } else {
                    refs.get(1).copied()
                };
                let change_now = self
                    .task_transition
                    .borrow()
                    .as_ref()
                    .is_some_and(|(at, _)| operation == Some(*at));
                if change_now && let Some((_, change)) = self.task_transition.take() {
                    change(self);
                }
                Ok(out(body))
            }
        }

        impl Services for SpyPanel {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                if !self.allow_board_queries.get() {
                    panic!("unexpected command {command:?}");
                }
                if self.canonical_assignment.get() {
                    if command == NativeCommand::Room
                        && args.first().is_some_and(|v| v == "leave")
                        && self
                            .task_transition
                            .borrow()
                            .as_ref()
                            .is_some_and(|(at, _)| *at == "room-leave")
                        && let Some((_, change)) = self.task_transition.take()
                    {
                        change(self);
                    }
                    let (code, mut stdout, stderr) = match command {
                        NativeCommand::Plan => oms_command_plan::run(args, repo, &self.stop),
                        NativeCommand::Room => oms_command_room::run(args, repo, &self.stop),
                        NativeCommand::Events => oms_command_operations::run(
                            oms_command_operations::App::Events,
                            args,
                            repo,
                            &self.stop,
                        ),
                        _ => panic!("unexpected bound assignment command {command:?}"),
                    };
                    if code == 0
                        && command == NativeCommand::Plan
                        && args.first().is_some_and(|v| v == "evidence-snapshot")
                        && let Some(overlay) = self.task_snapshot_override.borrow().as_ref()
                    {
                        let mut task: Value = serde_json::from_slice(&stdout).unwrap();
                        if task["id"] == overlay["id"] {
                            task.as_object_mut()
                                .unwrap()
                                .extend(overlay.as_object().unwrap().clone());
                            stdout = serde_json::to_vec(&task).unwrap();
                        }
                    }
                    if code == 0
                        && command == NativeCommand::Room
                        && args.first().is_some_and(|v| v == "leave")
                        && self
                            .task_transition
                            .borrow()
                            .as_ref()
                            .is_some_and(|(at, _)| *at == "room-leave-recorded")
                        && let Some((_, change)) = self.task_transition.take()
                    {
                        change(self);
                    }
                    if command == NativeCommand::Plan
                        && args.first().is_some_and(|v| v == "review")
                        && self
                            .task_transition
                            .borrow()
                            .as_ref()
                            .is_some_and(|(at, _)| *at == "plan-review")
                        && let Some((_, change)) = self.task_transition.take()
                    {
                        change(self);
                    }
                    return Ok(CommandOutput {
                        code,
                        stdout,
                        stderr,
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    });
                }
                let body = match command {
                    NativeCommand::Room => {
                        let assignment = args.get(1).is_some_and(|arg| arg == "--repo");
                        if assignment {
                            assert_eq!(repo, self.repo);
                            assert_eq!(
                                args,
                                [
                                    "show",
                                    "--repo",
                                    self.repo.to_str().unwrap(),
                                    "--id",
                                    &self.room,
                                    "--exact",
                                    "--json"
                                ]
                            );
                        }
                        let exact =
                            assignment || args == ["show", "--id", &self.room, "--exact", "--json"];
                        assert!(exact || args == ["show", "--id", &self.room, "--json"]);
                        self.room_queries.borrow_mut().push(exact);
                        if self.room_query_fails.get() {
                            return Ok(CommandOutput { code: 1, ..out("") });
                        }
                        let successor = self.room_successor.get();
                        json!({
                            "id": if exact { &self.room } else { successor.unwrap_or(&self.room) },
                            "closed": self.room_closed.get(),
                            "successor": if exact { successor } else { None },
                            "participants": [{
                                "participant": self.main_participant,
                                "role": "main",
                                "provider": self.main_owner.get(),
                                "joined": true
                            }]
                        })
                    }
                    NativeCommand::Events => {
                        if args == ["list", "--json"] {
                            assert_eq!(repo, self.repo);
                            self.event_lists.set(self.event_lists.get() + 1);
                            if self.real_events.get() {
                                self.assignment_events.set(self.assignment_events.get() + 1);
                                let (code, stdout, stderr) = oms_command_operations::run(
                                    oms_command_operations::App::Events,
                                    args,
                                    repo,
                                    &self.stop,
                                );
                                return Ok(CommandOutput {
                                    code,
                                    stdout,
                                    stderr,
                                    cleanup_complete: true,
                                    output_complete: true,
                                    limited: false,
                                });
                            }
                            return self
                                .event_projection
                                .borrow()
                                .as_ref()
                                .map(|rows| out(serde_json::to_vec(rows).unwrap()))
                                .ok_or_else(|| Error::new("fixture Events unavailable"));
                        }
                        assert_eq!(args, &["show", "--attempt", &self.main_attempt, "--json"]);
                        if self.real_events.get() {
                            self.assignment_events.set(self.assignment_events.get() + 1);
                            let (code, stdout, stderr) = oms_command_operations::run(
                                oms_command_operations::App::Events,
                                args,
                                repo,
                                &self.stop,
                            );
                            return Ok(CommandOutput {
                                code,
                                stdout,
                                stderr,
                                cleanup_complete: true,
                                output_complete: true,
                                limited: false,
                            });
                        }
                        json!({
                            "attempt_id": self.main_attempt,
                            "tool": "panel-main",
                            "provider": "codex",
                            "terminal": false,
                            "state": "working",
                            "refs": {
                                "panel_role": "main",
                                "panel_room_id": self.room
                            }
                        })
                    }
                    _ => panic!("unexpected command {command:?}"),
                };
                Ok(out(serde_json::to_vec(&body).unwrap()))
            }
        }

        fn make_accounts(
            entries: Vec<(&str, Vec<(&str, PathBuf)>)>,
        ) -> oms_panel_core::routing::Accounts {
            crate::orchestrator::account_binding::test_support::sample_accounts(
                entries
                    .into_iter()
                    .map(|(provider, list)| (provider, "ordered", list))
                    .collect(),
            )
        }

        fn owner_entry(pid: u32, birth: &str, uid: u32) -> Owner {
            Owner {
                pid,
                birth: birth.into(),
                uid,
            }
        }

        #[test]
        fn provider_session_proof_uses_one_exact_read_batch() {
            let tmp = TempDir::new("orch-session-proof");
            let mut spy = SpyPanel::new(private_dir(&tmp.0.join("repo\ncontinued")));
            spy.socket_path = "socket 한\ncontinued  ".into();
            let proof = spy.load().session_proof;
            assert_eq!(proof.session, "$1");
            assert_eq!(proof.server_pid, spy.server_pid);
            assert_eq!(
                proof.server_birth,
                terminals::process(spy.server_pid).unwrap().birth
            );
            assert_eq!(
                proof.socket_digest,
                oms_panel_core::digest("socket 한\ncontinued".as_bytes())
            );
            assert_eq!(
                spy.calls_named("display-message"),
                [[
                    "tmux",
                    "display-message",
                    "-p",
                    "-t",
                    "$1",
                    "#{session_id}\t#{pid}\t#{socket_path}\t.",
                    ";",
                    "show-options",
                    "-v",
                    "-t",
                    "$1",
                    "@oms_panel_repo",
                ]]
            );
            assert!(spy.calls_named("show-options").is_empty());

            let pid = spy.server_pid;
            let owner = spy.repo.display();
            let identity = format!("$1\t{pid}\tsocket\t.\n");
            for reply in [
                out(format!("$2\t{pid}\tsocket\t.\n{owner}\n")),
                out(format!("$1\t0\tsocket\t.\n{owner}\n")),
                out(format!("$1\t{pid}\t\t.\n{owner}\n")),
                out(format!("$1\t{pid}\tsocket\t.\nextra\t.\n{owner}\n")),
                out(format!("$1\t{pid}\tsocket\n{owner}\n")),
                out(format!("{identity}/foreign\n{owner}\n")),
                out(format!("{identity}{owner}")),
                out(identity.clone()),
                CommandOutput {
                    code: 1,
                    ..out(format!("{identity}{owner}\n"))
                },
                CommandOutput {
                    output_complete: false,
                    ..out(format!("{identity}{owner}\n"))
                },
            ] {
                spy.calls.borrow_mut().clear();
                spy.session_reply.replace(Some(reply));
                assert!(Panel::load_for_caller(&spy.repo, ROOM, "%1", &spy, &spy.stop).is_err());
                assert_eq!(
                    spy.calls.borrow().len(),
                    2,
                    "list panes and one failed proof"
                );
                assert!(spy.calls_named("show-options").is_empty());
            }

            for unusual in ["repo\t.\ncontinued", "repo\t."] {
                let spy = SpyPanel::new(private_dir(&tmp.0.join(unusual)));
                assert_eq!(spy.load().session_proof.session, "$1");
                assert_eq!(
                    spy.calls_named("display-message"),
                    [[
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        "$1",
                        "#{session_id}\t#{pid}\t#{socket_path}",
                    ]]
                );
                assert_eq!(
                    spy.calls_named("show-options"),
                    [["tmux", "show-options", "-v", "-t", "$1", "@oms_panel_repo",]]
                );
            }
        }

        #[test]
        fn provider_membership_stays_on_the_exact_room() {
            const CASE: &str = "OMS_TEST_PROVIDER_MEMBERSHIP_PANE";
            const TEST: &str =
                "orchestrator::tests::account_tests::provider_membership_stays_on_the_exact_room";
            let tmp = TempDir::new("orch-provider-room");
            let Ok(caller) = std::env::var(CASE) else {
                for pane in ["%1", "%2"] {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", TEST])
                        .env_clear()
                        .env("HOME", &tmp.0)
                        .env(CASE, pane)
                        .env("TMUX_PANE", pane)
                        .env("OMS_PANEL_MAIN_ATTEMPT", MAIN)
                        .env("OMS_ROOM_PARTICIPANT", MAIN)
                        .output()
                        .unwrap();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        output.status.success() && stdout.contains("1 passed"),
                        "{stdout}\n{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                return;
            };
            let spy = SpyPanel::new(private_dir(&tmp.0.join("repo")));
            spy.allow_board_queries.set(true);
            spy.leads.borrow_mut().push(FakeLead {
                window: "@4".into(),
                pane_id: "%4".into(),
                pid: 2001,
                active: false,
                provider: "claude".into(),
            });
            let panel = Panel::load_for_caller(&spy.repo, ROOM, &caller, &spy, &spy.stop).unwrap();
            assert!(panel.require_membership().is_ok());
            for (successor, closed, read_fails) in [
                (Some("room-successor"), false, false),
                (None, true, false),
                (None, false, true),
            ] {
                spy.room_successor.set(successor);
                spy.room_closed.set(closed);
                spy.room_query_fails.set(read_fails);
                assert!(panel.require_membership().is_err());
                if caller == "%1" {
                    for (provider, task) in [("codex", None), ("claude", Some("Plan this task"))] {
                        let mut args = vec!["--room", ROOM, "--orchestrator", provider];
                        if let Some(task) = task {
                            args.extend(["--task", task]);
                        }
                        let options = Options::parse(&commands::strings(&args)).unwrap();
                        assert!(
                            super::super::run(&spy.repo, provider, &options, &spy, &spy.stop)
                                .is_err()
                        );
                    }
                } else {
                    assert_eq!(panel.refresh_usage(&mut Observations::default()), 0);
                    assert!(panel.toggle_dock(Some("claude"), 8, Some("codex")).is_err());
                }
            }
            assert_eq!(*spy.room_queries.borrow(), [true; 10]);
            assert!(spy.mutating_calls().is_empty());
            assert!(spy.calls_named("capture-pane").is_empty());

            if caller == "%2" {
                spy.room_query_fails.set(false);
                spy.leads.borrow_mut().push(FakeLead {
                    window: "@1".into(),
                    pane_id: "%3".into(),
                    pid: 2002,
                    active: false,
                    provider: "antigravity".into(),
                });
                spy.dock_geometries.borrow_mut().extend([
                    (
                        "%1".into(),
                        format!(
                            "$1\t@1\t%1\t{}\t0\t0\t3\t59\t32\t0\t27\t59\t5\t{MAIN}\t.",
                            spy.main_pid
                        ),
                    ),
                    (
                        "%2".into(),
                        format!(
                            "$1\t@1\t%2\t{}\t0\t0\t3\t59\t32\t0\t0\t59\t17\t{MAIN}\t.",
                            spy.main_pid
                        ),
                    ),
                    (
                        "%3".into(),
                        format!("$1\t@1\t%3\t2002\t0\t0\t3\t59\t32\t0\t18\t59\t8\t{MAIN}\t."),
                    ),
                    (
                        "%4".into(),
                        "$1\t@4\t%4\t2001\t0\t0\t1\t59\t32\t0\t0\t59\t32\t\t.".into(),
                    ),
                ]);
                let panel =
                    Panel::load_for_caller(&spy.repo, ROOM, &caller, &spy, &spy.stop).unwrap();
                assert_eq!(panel.docked(), Some("antigravity"));
                let err = panel
                    .toggle_dock(Some("claude"), 8, Some("codex"))
                    .unwrap_err();
                assert_eq!(err.message, "make the panel larger to dock native chat");
                assert!(spy.mutating_calls().is_empty());
            }
        }

        #[test]
        fn native_dock_switch_and_reopen_reuse_the_owned_slot() {
            let tmp = TempDir::new("native-dock-geometry");
            for (width, height, lane, board_rows) in [(316, 90, 145, 29), (140, 130, 140, 36)] {
                let spy = SpyPanel::new(private_dir(&tmp.0.join(width.to_string())));
                spy.allow_board_queries.set(true);
                for (window, pane, pid, provider) in [
                    ("@1", "%3", 2002, "claude"),
                    ("@4", "%4", 2001, "codex"),
                    ("@5", "%5", 2003, "antigravity"),
                ] {
                    spy.leads.borrow_mut().push(FakeLead {
                        window: window.into(),
                        pane_id: pane.into(),
                        pid,
                        active: false,
                        provider: provider.into(),
                    });
                }
                let geometry = |window: &str, pane: &str, pid, panes, (left, top, cols, rows)| {
                    let attempt = if window == "@1" { MAIN } else { "" };
                    format!(
                        "$1\t{window}\t{pane}\t{pid}\t0\t0\t{panes}\t{width}\t{height}\t{left}\t{top}\t{cols}\t{rows}\t{attempt}\t."
                    )
                };
                let board =
                    |panes, rows| geometry("@1", "%2", spy.main_pid, panes, (0, 0, lane, rows));
                let main = |panes| {
                    let bounds = if lane == width {
                        (0, board_rows + 62, width, height - board_rows - 62)
                    } else {
                        (lane + 1, 0, width - lane - 1, height)
                    };
                    geometry("@1", "%1", spy.main_pid, panes, bounds)
                };
                let undocked = || [board(2, board_rows + 61), main(2)];
                spy.dock_geometries.borrow_mut().extend([
                    ("%1".into(), main(3)),
                    ("%2".into(), board(3, board_rows)),
                    (
                        "%3".into(),
                        geometry("@1", "%3", 2002, 3, (0, board_rows + 1, lane, 60)),
                    ),
                    (
                        "%4".into(),
                        geometry("@4", "%4", 2001, 1, (0, 0, width, height)),
                    ),
                    (
                        "%5".into(),
                        geometry("@5", "%5", 2003, 1, (0, 0, width, height)),
                    ),
                ]);
                let load =
                    || Panel::load_for_caller(&spy.repo, ROOM, "%2", &spy, &spy.stop).unwrap();
                for (previous, next, provider, pane) in [
                    (0, 1, "codex", "%4"),
                    (1, 2, "antigravity", "%5"),
                    (2, 0, "claude", "%3"),
                ] {
                    spy.calls.borrow_mut().clear();
                    spy.dock_after_break.replace(Some(undocked()));
                    assert_eq!(
                        load().toggle_dock(Some(provider), 12, None).unwrap(),
                        Some(provider)
                    );
                    assert_eq!(spy.calls_named("break-pane").len(), 1);
                    let joins = spy.calls_named("join-pane");
                    assert_eq!(
                        joins[0][..10],
                        [
                            "tmux",
                            "join-pane",
                            "-d",
                            "-v",
                            "-l",
                            "60",
                            "-s",
                            pane,
                            "-t",
                            "%2"
                        ]
                    );
                    assert_eq!(
                        spy.calls_named("select-pane"),
                        [["tmux", "select-pane", "-t", pane]]
                    );
                    let mut leads = spy.leads.borrow_mut();
                    let mut geometries = spy.dock_geometries.borrow_mut();
                    leads[previous].window = format!("@{}", 6 + previous);
                    let old = &leads[previous];
                    geometries.insert(
                        old.pane_id.clone(),
                        geometry(&old.window, &old.pane_id, old.pid, 1, (0, 0, width, height)),
                    );
                    leads[next].window = "@1".into();
                    let new = &leads[next];
                    geometries.insert(
                        new.pane_id.clone(),
                        geometry(
                            "@1",
                            &new.pane_id,
                            new.pid,
                            3,
                            (0, board_rows + 1, lane, 60),
                        ),
                    );
                    geometries.insert("%2".into(), board(3, board_rows));
                    geometries.insert("%1".into(), main(3));
                }

                spy.dock_after_break.replace(Some(undocked()));
                assert_eq!(load().toggle_dock(None, 0, None).unwrap(), None);
                let remembered = spy.dock_rows.borrow().clone();
                assert!(remembered.ends_with(":60"), "{remembered}");
                spy.leads.borrow_mut()[0].window = "@6".into();
                let lead = geometry("@6", "%3", 2002, 1, (0, 0, width, height));
                spy.dock_geometries
                    .borrow_mut()
                    .insert("%3".into(), lead.clone());
                let open = |rows: &str| {
                    spy.calls.borrow_mut().clear();
                    assert_eq!(
                        load().toggle_dock(Some("claude"), 12, None).unwrap(),
                        Some("claude")
                    );
                    assert_eq!(spy.calls_named("join-pane")[0][5], rows);
                    assert!(spy.dock_rows.borrow().is_empty());
                };
                open("60");
                // An externally removed dock leaves no preference to replay.
                open("12");

                let preference_scope = remembered.strip_suffix("60").unwrap();
                for invalid in [
                    remembered.replacen("$1", "$8", 1),
                    remembered.replacen("@1", "@8", 1),
                    remembered.replacen(MAIN, "other", 1),
                    format!("{preference_scope}0"),
                    format!("{preference_scope}invalid"),
                ] {
                    spy.dock_rows.replace(invalid);
                    open("12");
                }

                // A new canvas or a changed Board divider invalidates a genuine preference.
                spy.dock_rows.replace(remembered.clone());
                let canvas = format!("\t{width}\t{height}\t");
                let resized = format!("\t{}\t{height}\t", width + 20);
                spy.dock_geometries.borrow_mut().extend([
                    ("%1".into(), main(2).replacen(&canvas, &resized, 1)),
                    (
                        "%2".into(),
                        board(2, board_rows + 61).replacen(&canvas, &resized, 1),
                    ),
                ]);
                open("12");

                spy.dock_rows.replace(remembered);
                let (board_bounds, main_bounds) = if lane == width {
                    (
                        (0, 0, lane, board_rows + 60),
                        (0, board_rows + 61, width, height - board_rows - 61),
                    )
                } else {
                    ((0, 0, lane - 1, height), (lane, 0, width - lane, height))
                };
                spy.dock_geometries.borrow_mut().extend([
                    (
                        "%1".into(),
                        geometry("@1", "%1", spy.main_pid, 2, main_bounds),
                    ),
                    (
                        "%2".into(),
                        geometry("@1", "%2", spy.main_pid, 2, board_bounds),
                    ),
                ]);
                open("12");

                spy.calls.borrow_mut().clear();
                spy.dock_geometries.borrow_mut().insert(
                    "%1".into(),
                    main(2).replacen(&format!("\t{}\t", spy.main_pid), "\t63100\t", 1),
                );
                assert!(load().toggle_dock(Some("claude"), 12, None).is_err());
                assert!(spy.mutating_calls().is_empty());

                spy.dock_geometries
                    .borrow_mut()
                    .insert("%1".into(), main(2));
                spy.dock_geometries
                    .borrow_mut()
                    .insert("%3".into(), lead.replacen("$1", "$5", 1));
                assert!(load().toggle_dock(Some("claude"), 12, None).is_err());
                assert!(spy.mutating_calls().is_empty());

                spy.dock_geometries.borrow_mut().extend([
                    (
                        "%2".into(),
                        board(2, board_rows + 61).replacen("\t0\t0\t2\t", "\t0\t1\t2\t", 1),
                    ),
                    ("%3".into(), lead),
                ]);
                assert!(load().toggle_dock(Some("claude"), 12, None).is_err());
                assert!(spy.mutating_calls().is_empty());

                spy.dock_geometries.borrow_mut().extend([
                    ("%2".into(), board(2, board_rows + 61)),
                    ("%6".into(), geometry("@1", "%6", 2004, 3, (0, 0, lane, 1))),
                ]);
                let assert_no_dock = |provider, message| {
                    spy.calls.borrow_mut().clear();
                    assert_eq!(
                        load()
                            .toggle_dock(Some(provider), 12, None)
                            .unwrap_err()
                            .message,
                        message
                    );
                    assert!(spy.mutating_calls().is_empty());
                    assert!(spy.calls_named("select-pane").is_empty());
                };
                assert_no_dock(
                    "claude",
                    "native chat needs the managed Board and Main layout",
                );
                spy.dock_geometries.borrow_mut().remove("%6");

                spy.dock_geometries.borrow_mut().insert(
                    "%3".into(),
                    geometry("@6", "%3", 2002, 2, (0, 0, width, height)),
                );
                assert_no_dock(
                    "claude",
                    "native chat pane layout changed; select the provider again",
                );

                let dead = geometry("@6", "%3", 2002, 1, (0, 0, width, height))
                    .replacen("	0	0	1	", "	1	0	1	", 1);
                spy.dock_geometries.borrow_mut().insert("%3".into(), dead);
                assert_no_dock(
                    "claude",
                    "native chat needs an unzoomed managed pane layout",
                );

                // A Main replacement after the outgoing pane broke stops the incoming join.
                spy.leads.borrow_mut()[0].window = "@1".into();
                spy.dock_geometries.borrow_mut().extend([
                    ("%1".into(), main(3)),
                    ("%2".into(), board(3, board_rows)),
                    (
                        "%3".into(),
                        geometry("@1", "%3", 2002, 3, (0, board_rows + 1, lane, 60)),
                    ),
                ]);
                let [after_board, after_main] = undocked();
                spy.dock_after_break.replace(Some([
                    after_board,
                    after_main.replacen(&format!("\t{}\t", spy.main_pid), "\t63100\t", 1),
                ]));
                spy.calls.borrow_mut().clear();
                assert!(load().toggle_dock(Some("codex"), 12, None).is_err());
                assert_eq!(spy.calls_named("break-pane").len(), 1);
                assert!(spy.calls_named("join-pane").is_empty());

                // The owned nonadjacent pane can close, with no size remembered.
                spy.leads.borrow_mut()[0].window = "@1".into();
                spy.dock_geometries.borrow_mut().extend([
                    ("%1".into(), main(3)),
                    ("%2".into(), board(3, board_rows)),
                    (
                        "%3".into(),
                        geometry("@1", "%3", 2002, 3, (1, board_rows + 1, lane - 1, 60)),
                    ),
                ]);
                assert_no_dock(
                    "codex",
                    "native chat needs a dock immediately below the managed board",
                );
                spy.dock_rows.replace("old preference".into());
                assert_eq!(load().toggle_dock(None, 0, None).unwrap(), None);
                assert!(spy.dock_rows.borrow().is_empty());
            }
        }

        #[test]
        fn agy_pending_quota_closes_only_after_current_dialog() {
            let tmp = TempDir::new("agy-quota-dialog");
            let repo = private_dir(&tmp.0.join("repo"));
            let spy = SpyPanel::new(repo.clone());
            spy.allow_board_queries.set(true);
            spy.leads.borrow_mut().push(FakeLead {
                window: "@4".into(),
                pane_id: "%4".into(),
                pid: 2001,
                active: false,
                provider: "antigravity".into(),
            });
            let host = FakeHost::new(private_dir(&tmp.0.join("state")));
            let panel = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
            let pane = &panel.leads["antigravity"];
            let (identity, _) = panel.observation_state("antigravity", pane, &host);
            let mut readings = Observations::default();
            readings.panes.insert(
                "antigravity".into(),
                Observation {
                    identity,
                    pending: Some(Instant::now()),
                    ..Observation::default()
                },
            );
            let sample = |screen: &str, readings: &mut Observations| {
                spy.screens.borrow_mut().insert("%4".into(), screen.into());
                let mut report = json!({});
                panel.observe_with_host(readings, &mut report, false, &host);
                report
            };
            for screen in [
                AGY_QUOTA_MISORDERED,
                &format!("{AGY_QUOTA_VIEWPORT}\n> newer prompt"),
            ] {
                let bad = sample(screen, &mut readings);
                assert!(
                    bad["provider_status"]["antigravity"]
                        .get("weekly_used")
                        .is_none()
                );
            }
            assert!(readings.panes["antigravity"].pending.is_some());
            assert!(spy.calls_named("send-keys").is_empty());
            assert!(spy.room_queries.borrow().is_empty());

            let current = AGY_QUOTA_VIEWPORT.replacen("100.00%", "94.62%", 1);
            let valid = sample(&current, &mut readings);
            let quota = &valid["provider_status"]["antigravity"];
            assert!((quota["weekly_used"].as_f64().unwrap() - 5.38).abs() < 0.001);
            let observed = quota["weekly_native_observed_at"].as_f64().unwrap();
            assert!(quota.get("weekly_resets_at").is_none());
            assert!(
                crate::render::weekly_usage_detail(&valid, observed)
                    .contains("Source AGY Gemini quota · Reset unreported")
            );
            assert!(readings.panes["antigravity"].pending.is_none());
            sample(&current, &mut readings);
            assert_eq!(
                spy.calls_named("send-keys"),
                [["tmux", "send-keys", "-t", "%4", "Escape"]]
            );
            assert_eq!(*spy.room_queries.borrow(), [true]);
        }

        #[test]
        fn agy_footer_weekly_requires_owned_process_and_preserves_usage() {
            let tmp = TempDir::new("agy-footer-weekly");
            let repo = private_dir(&tmp.0.join("repo"));
            let spy = SpyPanel::new(repo.clone());
            spy.leads.borrow_mut().push(FakeLead {
                window: "@4".into(),
                pane_id: "%4".into(),
                pid: 2001,
                active: false,
                provider: "antigravity".into(),
            });
            let mut host = FakeHost::new(private_dir(&tmp.0.join("state")));
            host.owners.insert(
                2001,
                Owner {
                    pid: 2001,
                    uid: host.uid,
                    birth: "agy-birth-1".into(),
                },
            );
            let panel = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
            let mut readings = Observations::default();
            let rule = "────────────────────────";
            let footer_screen = |weekly: &str| {
                format!(
                    "prior\n{rule}\n>\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 31% | {weekly} | main\n"
                )
            };
            let sample =
                |screen: &str, report: &mut Value, readings: &mut Observations, h: &FakeHost| {
                    spy.screens.borrow_mut().insert("%4".into(), screen.into());
                    panel.observe_with_host(readings, report, false, h);
                };

            // Owned live AGY pane with intact OMS footer projects agy_footer_weekly only.
            let mut report = json!({"collection": {"ok": true}});
            sample(
                &footer_screen("Gemini weekly 99% left"),
                &mut report,
                &mut readings,
                &host,
            );
            assert_eq!(report["agy_footer_weekly"], json!({"remaining": "99%"}));
            assert!(
                report["provider_status"]["antigravity"]
                    .get("weekly_used")
                    .is_none()
            );

            // Bounded <1% and >99% survive intact without becoming weekly_used.
            for expected in ["<1%", ">99%"] {
                let mut bounded = json!({"collection": {"ok": true}});
                sample(
                    &footer_screen(&format!("Gemini weekly {expected} left")),
                    &mut bounded,
                    &mut readings,
                    &host,
                );
                assert_eq!(bounded["agy_footer_weekly"], json!({"remaining": expected}));
                assert!(
                    bounded["provider_status"]["antigravity"]
                        .get("weekly_used")
                        .is_none()
                );
            }

            // Holds, pending, failed collection, and nonfinal/malformed footers clear display.
            for held in [
                json!({"collection": {"ok": true, "pending": true}, "agy_footer_weekly": {"remaining": "99%"}}),
                json!({"collection": {"ok": false}, "agy_footer_weekly": {"remaining": "99%"}}),
                json!({"collection": {"ok": true}, "capacity_stale": true, "agy_footer_weekly": {"remaining": "99%"}}),
            ] {
                let mut held = held;
                sample(
                    &footer_screen("Gemini weekly 99% left"),
                    &mut held,
                    &mut readings,
                    &host,
                );
                assert!(held.get("agy_footer_weekly").is_none());
            }
            let mut bad_footer =
                json!({"collection": {"ok": true}, "agy_footer_weekly": {"remaining": "99%"}});
            sample(
                &format!("{}trailing\n", footer_screen("Gemini weekly 99% left")),
                &mut bad_footer,
                &mut readings,
                &host,
            );
            assert!(bad_footer.get("agy_footer_weekly").is_none());

            // Unowned / foreign process clears agy_footer_weekly.
            host.owners.insert(
                2001,
                Owner {
                    pid: 2001,
                    uid: host.uid + 1,
                    birth: "1".into(),
                },
            );
            let mut foreign =
                json!({"collection": {"ok": true}, "agy_footer_weekly": {"remaining": "99%"}});
            sample(
                &footer_screen("Gemini weekly 99% left"),
                &mut foreign,
                &mut readings,
                &host,
            );
            assert!(foreign.get("agy_footer_weekly").is_none());

            // Restore ownership; verified /usage preserves its value and timestamp alongside footer.
            host.owners.insert(
                2001,
                Owner {
                    pid: 2001,
                    uid: host.uid,
                    birth: "agy-birth-1".into(),
                },
            );
            let current_usage = AGY_QUOTA_VIEWPORT.replacen("100.00%", "94.62%", 1);
            let mut usage_report = json!({"collection": {"ok": true}});
            sample(&current_usage, &mut usage_report, &mut readings, &host);
            let observed =
                usage_report["provider_status"]["antigravity"]["weekly_native_observed_at"]
                    .as_f64()
                    .unwrap();
            let mut both = json!({"collection": {"ok": true}});
            sample(
                &footer_screen("Gemini weekly 99% left"),
                &mut both,
                &mut readings,
                &host,
            );
            assert_eq!(
                both["provider_status"]["antigravity"]["weekly_native_observed_at"],
                json!(observed)
            );
            assert!(
                (both["provider_status"]["antigravity"]["weekly_used"]
                    .as_f64()
                    .unwrap()
                    - 5.38)
                    .abs()
                    < 0.001
            );
            assert_eq!(both["agy_footer_weekly"], json!({"remaining": "99%"}));
            assert!(
                crate::render::weekly_usage_detail(&both, observed)
                    .contains("Source AGY Gemini quota · Reset unreported")
            );
            assert!(
                crate::render::weekly_usage_detail(&both, observed + 601.0)
                    .contains("Source AGY native footer · Age unknown · Reset unknown")
            );
        }

        #[test]
        fn admitted_native_claude_spread_uses_one_projection_and_never_ambient_quota() {
            use crate::orchestrator::account_binding::test_support::sample_accounts;
            const CASE: &str = "OMS_NATIVE_SPREAD_FIXTURE";
            const TEST: &str = "orchestrator::tests::account_tests::admitted_native_claude_spread_uses_one_projection_and_never_ambient_quota";
            let tmp = TempDir::new("native-claude-spread");
            if std::env::var_os(CASE).is_none() {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST])
                    .env_clear()
                    .env("HOME", &tmp.0)
                    .env(CASE, "1")
                    .env("TMUX_PANE", "%1")
                    .env("OMS_PANEL_MAIN_ATTEMPT", MAIN)
                    .env("OMS_ROOM_PARTICIPANT", MAIN)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success() && stdout.contains("1 passed"),
                    "{stdout}"
                );
                return;
            }
            let repo = private_dir(&tmp.0.join("repo"));
            let primary = private_dir(&tmp.0.join("primary-private"));
            let backup = private_dir(&tmp.0.join("backup-private"));
            let state = private_dir(&tmp.0.join("state"));
            let mut host = FakeHost::new(state.clone());
            host.accounts = Some(sample_accounts(vec![(
                "claude",
                "spread",
                vec![
                    ("missing", tmp.0.join("missing")),
                    ("primary", primary.clone()),
                    ("backup", backup.clone()),
                ],
            )]));
            let spy = SpyPanel::new(repo.clone());
            spy.allow_board_queries.set(true);
            let pid = u32::MAX;
            spy.spawn_pid.set(pid);
            host.owners
                .insert(pid, owner_entry(pid, "fixture:123", host.uid));
            host.owners.insert(
                spy.main_pid,
                owner_entry(spy.main_pid, "fixture-main", host.uid),
            );
            let worker = |seq: u32, provider: &str, account: &str, state: &str, terminal: bool| {
                json!({
                    "attempt_id":format!("att_{seq:032x}"),"provider":provider,"tool":"peer-delegate","state":state,"terminal":terminal,
                    "refs":{"panel_role":"worker","panel_room_id":"room-other-local","panel_account":account}
                })
            };
            let mut contradictory = worker(6, "claude", "claude:backup", "working", false);
            contradictory["refs"]["panel_provider"] = json!("codex");
            let projection = json!([
                {"attempt_id":MAIN,"provider":"codex","tool":"panel-main","terminal":false,"state":"working",
                 "refs":{"panel_role":"main","panel_room_id":ROOM,"panel_native_scheduling":"unmanaged"}},
                worker(1,"claude","claude:primary","working",false),
                worker(2,"codex","claude:backup","working",false),
                worker(3,"claude","claude:backup","done",true),
                worker(4,"claude","backup","working",false),
                worker(5,"claude","claude:backup","queued",false),
                contradictory,
                worker(7,"claude","claude:backup","working",false),
                worker(7,"claude","claude:backup","starting",false),
                worker(8,"claude","claude:unknown","working",false),
            ]);
            assert_eq!(claude_worker_load(&projection).get("primary"), Some(&1));
            assert!(!claude_worker_load(&projection).contains_key("backup"));
            let options = Options::parse(&commands::strings(&[
                "--orchestrator",
                "claude",
                "--spread-accounts",
                "--room",
                ROOM,
                "--model",
                "fixture/claude-model",
                "--reasoning-effort",
                "xhigh",
            ]))
            .unwrap();
            let start =
                |host: &FakeHost| run_with_host(&repo, "claude", &options, &spy, &spy.stop, host);
            assert!(start(&host).is_err());
            assert!(spy.mutating_calls().is_empty());
            spy.event_lists.set(0);
            spy.event_projection.replace(Some(projection));
            let (_, output, err) = start(&host).unwrap();
            assert_eq!(spy.event_lists.get(), 1);
            assert!(err.is_empty());
            let result: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(
                result,
                json!({"status":"started","provider":"claude","account":"backup",
                "account_pinned":false,"account_verification":"verified"})
            );
            assert!(!String::from_utf8_lossy(&output).contains(backup.to_str().unwrap()));
            let window = &spy.calls_named("new-window")[0];
            assert!(window.contains(&format!("CLAUDE_CONFIG_DIR={}", backup.display())));
            assert!(window.ends_with(&[
                "claude".into(),
                "--model".into(),
                "fixture/claude-model".into(),
                "--dangerously-skip-permissions".into(),
                "--effort".into(),
                "xhigh".into(),
                "--append-system-prompt".into(),
                STARTUP_PROMPT.into(),
            ]));
            assert!(
                window
                    .iter()
                    .all(|arg| !arg.starts_with("OMS_PANEL_CLAUDE_OBSERVATION="))
            );
            let calls = spy.mutating_calls();
            let tag = |value| {
                calls
                    .iter()
                    .position(|args| args.iter().any(|arg| arg == value))
                    .unwrap()
            };
            assert!(tag("@oms_claude_quota_bound") < tag("@oms_provider_session"));
            let panel = spy.load();
            let (_, binding) = panel.observation_state("claude", &panel.leads["claude"], &host);
            let launch = binding.unwrap().claude_auto_observation;
            let expected = launch
                .as_ref()
                .map_or_else(|| "auto".into(), |proof| proof.marker());
            assert_eq!(panel.claude_quota_mark.as_deref(), Some(expected.as_str()));
            let switch = crate::claude_lead_usage::AUTO_OBSERVATION_SWITCH;
            assert!(
                window
                    .windows(2)
                    .any(|pair| pair[0] == "-u" && pair[1] == switch)
            );
            if let Some(launch) = launch {
                assert!(window.contains(&format!("{switch}={}", launch.switch_value())));
                assert!(!String::from_utf8_lossy(&output).contains(&launch.switch_value()));
            }

            spy.screens.borrow_mut().insert(
                "%4".into(),
                "Settings Status Config Usage Stats\nCurrent week (all models)\n37% used".into(),
            );
            let withheld = |host: &FakeHost| {
                let mut report = accepted_codex_scope();
                report["provider_status"]["claude"] = json!({"source":"HUD cache","weekly_used":30,"five_hour_used":51,"account_label":"primary"});
                report["claude_local_weekly"] = json!({"weekly_used":9});
                spy.load().observe_with_claude_reader(
                    &mut Observations::default(),
                    &mut report,
                    false,
                    host,
                    |_, _, _| panic!("shared HUD cannot prove the automatic account source"),
                );
                let status = &report["provider_status"]["claude"];
                assert!(report.get("claude_local_weekly").is_none());
                assert_eq!(status["source"], "unavailable");
                assert!(
                    status.get("weekly_used").is_none()
                        && status.get("five_hour_used").is_none()
                        && status.get("account_label").is_none()
                );
            };
            withheld(&host);
            spy.claude_quota_marks.borrow_mut().clear();
            withheld(&host);
            spy.claude_quota_marks
                .borrow_mut()
                .insert("%4".into(), "auto".into());
            host.accounts = Some(sample_accounts(vec![(
                "claude",
                "spread",
                vec![("backup", primary)],
            )]));
            withheld(&host);
            let mutations = spy.mutating_calls();
            assert!(
                start(&host)
                    .unwrap_err()
                    .message
                    .contains("already connected")
            );
            assert_eq!(spy.mutating_calls(), mutations);
            let ordinary = Options::parse(&commands::strings(&[
                "--orchestrator",
                "claude",
                "--spread-accounts",
                "--room",
                ROOM,
            ]))
            .unwrap();
            let (_, output, _) =
                run_with_host(&repo, "claude", &ordinary, &spy, &spy.stop, &host).unwrap();
            let reused: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(reused["status"], "connected");
            assert_eq!(reused["account"], "backup");
            assert_eq!(spy.calls_named("new-window").len(), 1);
            let bindings = state
                .join("oh-my-setting/panel-control")
                .join(account_binding::path_digest(&repo))
                .join("account-bindings");
            let record = std::fs::read_dir(bindings)
                .unwrap()
                .flatten()
                .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .unwrap()
                .path();
            std::fs::remove_file(record).unwrap();
            withheld(&host);
        }

        #[test]
        fn pinned_claude_quota_never_uses_unproved_ambient_or_dialog() {
            let tmp = TempDir::new("pinned-claude-quota");
            let repo = private_dir(&tmp.0.join("repo"));
            let home = private_dir(&tmp.0.join("pinned-home"));
            let state = private_dir(&tmp.0.join("state"));
            let mut host = FakeHost::new(state.clone());
            let registry = |path: &Path| {
                Some(make_accounts(vec![(
                    "claude",
                    vec![("pinned", path.to_owned())],
                )]))
            };
            host.accounts = registry(&home);
            let spy = SpyPanel::new(repo.clone());
            let pid = u32::MAX; // Never open a real process environment from this fixture.
            let launch_owner = owner_entry(pid, "fixture:123", host.uid);
            host.owners.insert(pid, launch_owner.clone());
            host.owners.insert(
                spy.main_pid,
                owner_entry(spy.main_pid, "fixture-main", host.uid),
            );
            spy.spawn_pid.set(pid);
            spy.load()
                .execute_with_host("claude", Some("pinned"), None, &host)
                .unwrap();
            let calls = spy.mutating_calls();
            let pos = |tag| {
                calls
                    .iter()
                    .position(|args| args.iter().any(|arg| arg == tag))
                    .unwrap()
            };
            assert!(pos("@oms_claude_quota_bound") < pos("@oms_provider_session"));
            assert_eq!(spy.load().claude_quota_mark.as_deref(), Some("1"));
            let dialog = "Settings Status Config Usage Stats\nCurrent week (all models)\n37% used";
            spy.screens.borrow_mut().insert("%4".into(), dialog.into());
            let now = oms_panel_core::timestamp() as f64;
            let cached_at = now - 1.0;
            let decoy = json!({
                "source":"HUD cache", "account_label":"ambient", "observed_at":cached_at,
                "weekly_used":70, "five_hour_used":80, "weekly_resets_at":now+1800.0,
                "five_hour_resets_at":now+900.0, "resets_at":now+600.0,
                "weekly_native_observed_at":cached_at, "weekly_lead_observed_at":cached_at,
                "short_windows":[{"minutes":60,"used":50}], "context_left":41,
                "accounts":{"pinned":{"weekly_used":null}}
            });
            let codex = json!({"weekly_used":8, "account_label":"codex-account"});
            let observe =
                |host: &FakeHost, reader: &dyn Fn(&Owner, &str, f64) -> Option<LeadReading>| {
                    let mut report = accepted_codex_scope();
                    report["provider_status"]["claude"] = decoy.clone();
                    report["provider_status"]["codex"] = codex.clone();
                    spy.load().observe_with_claude_reader(
                        &mut Observations::default(),
                        &mut report,
                        false,
                        host,
                        reader,
                    );
                    assert_eq!(report["provider_status"]["codex"], codex);
                    report
                };
            let suppressed = |report: &Value| {
                let status = &report["provider_status"]["claude"];
                for key in [
                    "weekly_used",
                    "five_hour_used",
                    "short_windows",
                    "account_label",
                    "weekly_resets_at",
                    "five_hour_resets_at",
                    "resets_at",
                    "weekly_native_observed_at",
                    "weekly_lead_observed_at",
                ] {
                    assert!(status.get(key).is_none(), "unproved {key}");
                }
                assert_eq!(status["context_left"], 41);
            };
            let valid = LeadReading {
                weekly_used: Some(14.0),
                resets_at: Some(now + 1000.0),
                observed_at: now - 40.0,
                stale: false,
            };
            let bound_digest = account_binding::path_digest(&home);
            let read_valid = |owner: &Owner, digest: &str, at| {
                assert_eq!(owner, &launch_owner);
                assert!(digest == bound_digest && at >= now);
                let mut leads = spy.leads.borrow_mut();
                leads[0].active = !leads[0].active;
                Some(valid.clone())
            };
            let report = observe(&host, &read_valid);
            let status = &report["provider_status"]["claude"];
            assert_eq!(status["weekly_used"], 14.0);
            assert_eq!(status["weekly_resets_at"], now + 1000.0);
            assert_eq!(status["account_label"], "pinned");
            assert_eq!(status["observed_at"], now - 40.0);
            assert_eq!(status["accounts"]["pinned"]["weekly_used"], Value::Null);
            for key in [
                "five_hour_used",
                "five_hour_resets_at",
                "short_windows",
                "resets_at",
                "weekly_native_observed_at",
            ] {
                assert!(status.get(key).is_none(), "ambient {key}");
            }
            let line =
                crate::render::frame(&report, &Default::default(), 120, 24, &Default::default())
                    .join("\n");
            assert!(line.contains("Claude 86%") && !line.contains("Claude 30%"));
            let public = report.to_string();
            assert!(!public.contains(&bound_digest) && !public.contains(home.to_str().unwrap()));

            suppressed(&observe(&host, &|_, _, _| None));
            let stale = observe(&host, &|_, _, _| {
                Some(LeadReading {
                    weekly_used: None,
                    resets_at: None,
                    observed_at: now - 800.0,
                    stale: true,
                })
            });
            suppressed(&stale);
            assert_eq!(stale["provider_status"]["claude"]["source"], "cache stale");

            let replaced = observe(&host, &|_, _, _| {
                spy.leads.borrow_mut()[0].pid = pid - 1;
                Some(valid.clone())
            });
            suppressed(&replaced);
            spy.leads.borrow_mut()[0].pid = pid;
            let no_hud =
                |_: &Owner, _: &str, _: f64| panic!("unproved lead reached the HUD reader");
            host.owners.remove(&pid);
            suppressed(&observe(&host, &no_hud));
            host.owners
                .insert(pid, owner_entry(pid, "reused", host.uid));
            suppressed(&observe(&host, &no_hud));
            host.owners.insert(pid, launch_owner.clone());
            let remapped = private_dir(&tmp.0.join("remapped-home"));
            let displaced = tmp.0.join("displaced-home");
            suppressed(&observe(&host, &|_, _, _| {
                std::fs::rename(&home, &displaced).unwrap();
                std::os::unix::fs::symlink(&remapped, &home).unwrap();
                Some(valid.clone())
            }));
            std::fs::remove_file(&home).unwrap();
            std::fs::rename(&displaced, &home).unwrap();
            assert!(
                spy.load()
                    .observation_generation_with_host(&host)
                    .unwrap()
                    .lead_accounts[1]
                    .is_some()
            );
            host.accounts = registry(&remapped);
            assert!(spy.load().observation_generation_with_host(&host).is_err());
            suppressed(&observe(&host, &no_hud));
            host.accounts = None;
            suppressed(&observe(&host, &no_hud));
            host.accounts = registry(&home);
            spy.claude_quota_marks
                .borrow_mut()
                .insert("%4".into(), "invalid".into());
            suppressed(&observe(&host, &no_hud));
            spy.claude_quota_marks
                .borrow_mut()
                .insert("%4".into(), "1".into());

            let bindings = state
                .join("oh-my-setting/panel-control")
                .join(account_binding::path_digest(&repo))
                .join("account-bindings");
            let record = std::fs::read_dir(&bindings)
                .unwrap()
                .flatten()
                .find(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
                .unwrap()
                .path();
            std::fs::write(&record, b"bad receipt").unwrap();
            suppressed(&observe(&host, &no_hud));
            std::fs::remove_file(&record).unwrap();
            suppressed(&observe(&host, &no_hud));

            spy.leads.borrow_mut().clear();
            spy.main_owner.set("claude");
            spy.main_account_ready.set(true);
            suppressed(&observe(&host, &no_hud));
            spy.main_account_ready.set(false);
            assert_eq!(observe(&host, &no_hud)["provider_status"]["claude"], decoy);

            host.accounts = None;
            let launch = spy
                .load()
                .execute_with_host("claude", None, None, &host)
                .unwrap();
            assert_eq!(launch["account_verification"], "unpinned_ambient");
            assert_eq!(spy.load().claude_quota_mark, None);
            spy.screens.borrow_mut().insert("%5".into(), dialog.into());
            let report = observe(&host, &no_hud);
            let unpinned = &report["provider_status"]["claude"];
            let observed_at = unpinned["weekly_native_observed_at"].as_f64().unwrap();
            assert!(observed_at >= now);
            let mut expected = decoy;
            expected["weekly_used"] = json!(37.0);
            expected["weekly_native_observed_at"] = json!(observed_at);
            assert_eq!(unpinned, &expected);
        }

        #[test]
        fn pinned_standalone_claude_main_uses_only_its_direct_child_hud() {
            let tmp = TempDir::new("pinned-claude-main-quota");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let home = private_dir(&tmp.0.join("selected"));
            let other = private_dir(&tmp.0.join("remapped"));
            let mut host = FakeHost::new(state.clone());
            let registry = |path: &Path| {
                Some(make_accounts(vec![(
                    "claude",
                    vec![("pinned", path.to_owned())],
                )]))
            };
            host.accounts = registry(&home);
            let spy = SpyPanel::new(repo.clone());
            spy.main_owner.set("claude");
            spy.main_account_ready.set(true);
            spy.allow_board_queries.set(true);
            let wrapper = owner_entry(spy.main_pid, "fixture-wrapper", host.uid);
            let child = owner_entry(901_001, "fixture:123", host.uid);
            host.owners.insert(wrapper.pid, wrapper.clone());
            host.owners.insert(child.pid, child.clone());
            let panel = spy.load();
            let anchor = panel.binding_anchor("claude", &panel.main, &wrapper.birth);
            let receipt =
                account_binding::resolve_main_selection(&host, "claude", None, "pinned", "")
                    .unwrap();
            account_binding::save_binding(&host, "main", &anchor, &receipt).unwrap();
            let directory = state
                .join("oh-my-setting/panel-control")
                .join(account_binding::path_digest(&repo))
                .join("account-bindings");
            assert!(
                panel
                    .observation_generation_with_host(&host)
                    .unwrap()
                    .main_account
                    .is_some()
            );
            let mut remapped = host.clone();
            remapped.accounts = registry(&other);
            assert!(panel.observation_generation_with_host(&remapped).is_err());
            let record = std::fs::read_dir(directory)
                .unwrap()
                .flatten()
                .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .unwrap()
                .path();
            let legacy = std::fs::read(&record).unwrap();

            let now = oms_panel_core::timestamp() as f64;
            let mut report = accepted_codex_scope();
            report["room"]["participants"][0]["provider"] = json!("claude");
            report["room_attempts"][MAIN]["provider"] = json!("claude");
            report["room_attempts"][MAIN]["refs"]["panel_owner"] = json!("claude");
            report["room_attempts"][MAIN]["refs"]["panel_harness"] = json!("claude");
            let ambient = json!({
                "source":"HUD cache", "account_label":"ambient", "observed_at":now,
                "weekly_used":70, "five_hour_used":80, "weekly_resets_at":now+1800.0,
                "five_hour_resets_at":now+900.0, "short_windows":[{"minutes":60,"used":50}],
                "weekly_lead_observed_at":now, "weekly_native_observed_at":now
            });
            report["provider_status"]["claude"] = ambient.clone();
            let digest = account_binding::path_digest(&home);
            let valid = LeadReading {
                weekly_used: Some(14.0),
                resets_at: Some(now + 1000.0),
                observed_at: now - 40.0,
                stale: false,
            };
            let read_valid = |owner: &Owner, configured: &str, at| {
                assert_eq!(owner, &child);
                assert!(configured == digest && at >= now);
                Some(valid.clone())
            };
            let observe =
                |host: &FakeHost, reader: &dyn Fn(&Owner, &str, f64) -> Option<LeadReading>| {
                    let mut report = report.clone();
                    spy.load().observe_with_claude_reader(
                        &mut Observations::default(),
                        &mut report,
                        false,
                        host,
                        reader,
                    );
                    report
                };
            let suppressed = |report: &Value| {
                let status = &report["provider_status"]["claude"];
                assert_eq!(status["source"], "unavailable");
                for key in [
                    "weekly_used",
                    "weekly_resets_at",
                    "weekly_lead_observed_at",
                    "weekly_native_observed_at",
                    "observed_at",
                    "five_hour_used",
                    "five_hour_resets_at",
                    "short_windows",
                    "account_label",
                ] {
                    assert!(status.get(key).is_none(), "unproved {key}");
                }
            };
            let no_hud =
                |_: &Owner, _: &str, _: f64| panic!("unproved child reached the HUD reader");
            for pid in [wrapper.pid, child.pid + 1] {
                account_binding::record_claude_main_child(&host, &anchor, &receipt, pid);
                assert_eq!(std::fs::read(&record).unwrap(), legacy);
            }
            suppressed(&observe(&host, &no_hud));
            account_binding::record_claude_main_child(&host, &anchor, &receipt, child.pid);
            let actual = observe(&host, &read_valid);
            let status = &actual["provider_status"]["claude"];
            assert_eq!(status["weekly_used"], 14.0);
            assert_eq!(status["account_label"], "pinned");
            assert_eq!(status["weekly_resets_at"], now + 1000.0);
            assert_eq!(status["observed_at"], now - 40.0);
            assert_eq!(status["source"], "HUD cache");
            for key in [
                "weekly_lead_observed_at",
                "weekly_native_observed_at",
                "five_hour_used",
                "five_hour_resets_at",
                "short_windows",
            ] {
                assert!(status.get(key).is_none(), "unproved {key}");
            }
            let details = crate::render::weekly_usage_detail(&actual, now);
            assert!(details.contains("86% left") && details.contains("HUD cache"));
            let public = actual.to_string();
            for private in [
                "claude_main_child",
                &child.birth,
                &digest,
                home.to_str().unwrap(),
            ] {
                assert!(!public.contains(private));
            }

            suppressed(&observe(&host, &|_, _, _| None));
            for bad in [
                LeadReading {
                    observed_at: now - 900.0,
                    stale: true,
                    ..valid.clone()
                },
                LeadReading {
                    resets_at: Some(now - 1.0),
                    ..valid.clone()
                },
            ] {
                suppressed(&observe(&host, &|_, _, _| Some(bad.clone())));
            }
            for pid in [child.pid, wrapper.pid] {
                host.reused_pid.set(Some(pid));
                suppressed(&observe(&host, &no_hud));
                host.reused_pid.set(None);
                suppressed(&observe(&host, &|owner, root, at| {
                    host.reused_pid.set(Some(pid));
                    read_valid(owner, root, at)
                }));
                host.reused_pid.set(None);
            }
            host.owners.remove(&child.pid);
            suppressed(&observe(&host, &no_hud));
            host.owners.insert(child.pid, child.clone());
            host.accounts = registry(&other);
            suppressed(&observe(&host, &no_hud));
            host.accounts = registry(&home);
            suppressed(&observe(&host, &|owner, root, at| {
                spy.room_closed.set(true);
                read_valid(owner, root, at)
            }));
            spy.room_closed.set(false);

            let lead = owner_entry(u32::MAX, "fixture-lead", host.uid);
            host.owners.insert(lead.pid, lead.clone());
            spy.spawn_pid.set(lead.pid);
            suppressed(&observe(&host, &|owner, root, at| {
                spy.load()
                    .execute_with_host("claude", Some("pinned"), None, &host)
                    .unwrap();
                read_valid(owner, root, at)
            }));
            let actual = observe(&host, &|owner, _, _| {
                assert_eq!(owner, &lead);
                Some(LeadReading {
                    weekly_used: Some(45.0),
                    ..valid.clone()
                })
            });
            assert_eq!(actual["provider_status"]["claude"]["weekly_used"], 45.0);
            assert_eq!(
                actual["provider_status"]["claude"]["weekly_lead_observed_at"],
                valid.observed_at
            );
            spy.leads.borrow_mut().clear();

            std::fs::write(&record, b"bad receipt").unwrap();
            account_binding::record_claude_main_child(&host, &anchor, &receipt, child.pid);
            assert_eq!(std::fs::read(&record).unwrap(), b"bad receipt");
            suppressed(&observe(&host, &no_hud));
            std::fs::remove_file(&record).unwrap();
            suppressed(&observe(&host, &no_hud));
            spy.main_account_ready.set(false);
            assert_eq!(
                observe(&host, &no_hud)["provider_status"]["claude"],
                ambient
            );
        }

        fn accepted_codex_scope() -> Value {
            json!({
                "collection":{"ok":true},
                "room":{"id":ROOM,"closed":false,"participants":[
                    {"participant":MAIN,"role":"main","provider":"codex","joined":true}]},
                "room_attempts":{(MAIN):{
                    "attempt_id":MAIN,"tool":"panel-main","provider":"codex","state":"working","terminal":false,
                    "refs":{"panel_role":"main","panel_room_id":ROOM}}}
            })
        }

        #[test]
        fn pinned_launch_injects_env_preserves_startup_prompt_and_leaks_no_paths() {
            let tmp = TempDir::new("orch-pinned");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-secret-home-a"));
            let claude_a = private_dir(&tmp.0.join("claude-secret-home-a"));
            let uid = rustix::process::getuid().as_raw();

            let mut host = FakeHost::new(state.clone());
            host.accounts = Some(make_accounts(vec![
                ("codex", vec![("acct-a", codex_a.clone())]),
                ("claude", vec![("acct-a", claude_a.clone())]),
            ]));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));
            host.owners.insert(2002, owner_entry(2002, "p2002", uid));

            let spy = SpyPanel::new(repo.clone());

            // 1. Pinned Codex launch
            let res_codex = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();
            assert_eq!(res_codex["status"], "started");
            assert_eq!(res_codex["provider"], "codex");
            assert_eq!(res_codex["account"], "acct-a");
            assert_eq!(res_codex["account_pinned"], true);
            assert_eq!(res_codex["account_verification"], "verified");
            let res_codex_str = res_codex.to_string();
            assert!(!res_codex_str.contains(codex_a.to_str().unwrap()));

            // 2. Pinned Claude launch
            spy.spawn_pid.set(2002);
            let res_claude = spy
                .load()
                .execute_with_host("claude", Some("acct-a"), None, &host)
                .unwrap();
            assert_eq!(res_claude["status"], "started");
            assert_eq!(res_claude["account"], "acct-a");
            assert_eq!(res_claude["account_pinned"], true);
            assert_eq!(res_claude["account_verification"], "verified");
            assert!(!res_claude.to_string().contains(claude_a.to_str().unwrap()));

            let windows = spy.calls_named("new-window");
            assert_eq!(windows.len(), 2);

            let codex_cmd = &windows[0];
            assert!(
                codex_cmd
                    .windows(2)
                    .any(|w| w == ["-F", "#{pane_id}\t#{pane_pid}"])
            );
            assert!(codex_cmd.windows(2).any(|w| w == ["-u", "CODEX_HOME"]));
            assert!(codex_cmd.contains(&format!("CODEX_HOME={}", codex_a.display())));
            assert!(codex_cmd.contains(&"OMS_HARNESS_CHILD=1".to_owned()));
            assert!(codex_cmd.contains(&"OMS_HARNESS_DELEGATE_DEPTH=1".to_owned()));

            let claude_cmd = &windows[1];
            assert!(
                claude_cmd
                    .windows(2)
                    .any(|w| w == ["-u", "CLAUDE_CONFIG_DIR"])
            );
            assert!(claude_cmd.contains(&format!("CLAUDE_CONFIG_DIR={}", claude_a.display())));
            assert!(claude_cmd.contains(&"OMS_HARNESS_CHILD=1".to_owned()));
            assert!(claude_cmd.contains(&"OMS_HARNESS_DELEGATE_DEPTH=1".to_owned()));
            assert!(
                claude_cmd
                    .windows(2)
                    .any(|w| w == ["--append-system-prompt", STARTUP_PROMPT]),
                "Claude startup prompt injection must be preserved"
            );

            // Neither tmux set-option nor persisted files may contain raw home paths.
            for opt in spy.calls_named("set-option") {
                let joined = opt.join(" ");
                assert!(!joined.contains(codex_a.to_str().unwrap()));
                assert!(!joined.contains(claude_a.to_str().unwrap()));
            }
            let ns = account_binding::path_digest(&repo);
            let bind_dir = state
                .join("oh-my-setting/panel-control")
                .join(ns)
                .join("account-bindings");
            for entry in std::fs::read_dir(&bind_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    let body = std::fs::read_to_string(&path).unwrap();
                    assert!(!body.contains(codex_a.to_str().unwrap()));
                    assert!(!body.contains(claude_a.to_str().unwrap()));
                }
            }
        }

        fn alternate_draft(text: &str) -> (String, (usize, usize)) {
            use std::fmt::Write as _;
            let mut draft = String::new();
            let mut rest = text;
            let mut row = 0;
            while rest.len() > 76 {
                let end = rest
                    .char_indices()
                    .take_while(|(index, _)| *index < 76)
                    .filter_map(|(index, ch)| (ch == ' ').then_some(index + 1))
                    .last()
                    .unwrap_or_else(|| rest.floor_char_boundary(76));
                let prefix = if row == 0 { "› " } else { "  " };
                writeln!(draft, "{prefix}{}", &rest[..end]).unwrap();
                rest = &rest[end..];
                row += 1;
            }
            let prefix = if row == 0 { "› " } else { "  " };
            write!(draft, "{prefix}{rest}\n{CODEX_STATUS}").unwrap();
            (draft, (crate::render::width(rest) + 2, row))
        }

        fn alternate_fixture() -> (TempDir, SpyPanel, FakeHost, AlternateCaller, PathBuf) {
            let tmp = TempDir::new("orch-alternate");
            let (spy, host, caller, home) = alternate_fixture_at(&tmp.0);
            (tmp, spy, host, caller, home)
        }

        fn alternate_fixture_at(root: &Path) -> (SpyPanel, FakeHost, AlternateCaller, PathBuf) {
            let spy = SpyPanel::new(private_dir(&root.join("repo")));
            let mut host = FakeHost::new(private_dir(&root.join("state")));
            let first = private_dir(&root.join("first-home"));
            let second = private_dir(&root.join("second-home"));
            host.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("first", first), ("second", second.clone())],
            )]));
            for (pid, birth) in [
                (spy.main_pid, "main"),
                (2001, "primary"),
                (2002, "alternate"),
                (2003, "replacement"),
            ] {
                host.owners.insert(pid, owner_entry(pid, birth, host.uid));
            }
            spy.allow_board_queries.set(true);
            spy.event_projection.replace(Some(json!([{
                "attempt_id": MAIN, "tool": "panel-main", "provider": "codex", "terminal": false,
                "state": "working", "refs": {
                    "panel_role": "main", "panel_room_id": ROOM, "panel_native_scheduling": "unmanaged"
                }
            }])));
            let caller = AlternateCaller {
                repo: spy.repo.clone(),
                room: ROOM.into(),
                attempt: MAIN.into(),
                participant: MAIN.into(),
                owner: "codex".into(),
                campaign: false,
            };
            spy.load()
                .execute_with_host("codex", Some("first"), None, &host)
                .unwrap();
            spy.spawn_pid.set(2002);
            spy.calls.borrow_mut().clear();
            (spy, host, caller, second)
        }

        fn reset_alternate_assignment(spy: &SpyPanel) {
            spy.calls.borrow_mut().clear();
            spy.room_queries.borrow_mut().clear();
            spy.event_lists.set(0);
            spy.assignment_events.set(0);
            spy.screens
                .borrow_mut()
                .insert("%5".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            spy.alternate_task_cursor.set((2, 0));
        }

        #[test]
        fn codex_alternate_draft_and_receipt_require_visible_exact_text() {
            let prompt = format!("Assign {}", "x".repeat(90));
            let (screen, (x, y)) = alternate_draft(&prompt);
            let mut composer = CodexComposer {
                command: "codex".into(),
                screen,
                cursor_x: x as u32,
                cursor_y: y,
            };
            assert!(composer.exact_draft(&prompt));
            composer.screen = composer.screen.replacen("Assign \n", "Assign\n", 1);
            assert!(!composer.exact_draft(&prompt));
            composer.screen = format!("{CODEX_OPAQUE_PASTE}{CODEX_STATUS}");
            composer.cursor_y = 0;
            assert!(!composer.exact_draft(&prompt));
            composer.screen = format!("› \x1b[7mAssign\x1b[0m{CODEX_STATUS}");
            composer.cursor_x = 8;
            assert!(!composer.exact_draft("Assign"));

            composer.cursor_x = 2;
            composer.cursor_y = 3;
            for (id, accepted) in [(format!("{TURN_ID}0"), false), (TURN_ID.to_owned(), true)] {
                composer.screen =
                    format!("› New assignment Turn\n  {id}.\n\n{CODEX_EMPTY}{CODEX_STATUS}");
                assert_eq!(composer.acknowledged(TURN_ID), accepted);
            }
        }

        #[test]
        fn codex_alternate_assignment_routes_one_literal_task() {
            const CASE: &str = "OMS_ALTERNATE_TASK_ENTRY_TEST";
            const TEST: &str = "orchestrator::tests::account_tests::codex_alternate_assignment_routes_one_literal_task";
            const TASK: &str = r#"Report on {"read":"factual"} with café and literal $(text);"#;
            if std::env::var_os(CASE).is_none() {
                let tmp = TempDir::new("orch-alternate-entry");
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env_clear()
                    .env("HOME", &tmp.0)
                    .env(CASE, "1")
                    .env("OMS_ALTERNATE_TASK_ENTRY_ROOT", &tmp.0)
                    .env("OMS_ROOM_REPO", tmp.0.join("repo"))
                    .env("TMUX_PANE", "%1")
                    .env("OMS_ROOM_ID", ROOM)
                    .env("OMS_PANEL_MAIN_ATTEMPT", MAIN)
                    .env("OMS_ROOM_PARTICIPANT", MAIN)
                    .env("OMS_PANEL_OWNER", "codex")
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success() && stdout.contains("1 passed"),
                    "stdout:\n{stdout}\nstderr:\n{stderr}"
                );
                return;
            }
            // This invocation passes the real CLI entry point an exact synthetic Main context.
            let root = PathBuf::from(std::env::var_os("OMS_ALTERNATE_TASK_ENTRY_ROOT").unwrap());
            let (spy, host, caller, home) = alternate_fixture_at(&root);
            write_synthetic_ledger(&spy.repo, 4096);
            spy.real_events.set(true);
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            spy.load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap();
            drop(tx);

            let options = Options::parse(&commands::strings(&[
                "--orchestrator",
                "codex",
                "--lead",
                "alternate",
                "--account",
                "second",
                "--task",
                TASK,
            ]))
            .unwrap();
            let mut turns = BTreeSet::new();
            for _ in 0..2 {
                reset_alternate_assignment(&spy);
                let (_, output, _) =
                    run_with_host(&spy.repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&output).unwrap(),
                    alternate_result("submitted", Some("second".into()))
                );
                let writes = spy.mutating_calls();
                assert_eq!(writes.len(), 3, "{writes:?}");
                let prompt = &writes[0][5];
                assert!(prompt.starts_with("Task from Overall Main."));
                assert!(prompt.contains("Delegate substantive work to native children."));
                assert!(
                    prompt
                        .contains("Make only necessary integration or unblocking edits yourself.")
                );
                assert!(prompt.contains(&assignment_locator(&spy.repo, ROOM).unwrap()));
                assert!(prompt.ends_with(literal_tmux_arg(&format!("Task: {TASK}")).as_ref()));
                assert!(prompt.chars().count() <= CODEX_VISIBLE_PASTE_CHARS);
                assert!(!prompt.contains(home.to_str().unwrap()));
                let (_, turn) = prompt.split_once(" Turn ").unwrap();
                let (turn, _) = turn.split_once(". ").unwrap();
                assert_eq!(turn.len(), 20);
                assert!(turns.insert(turn.to_owned()));
                assert_eq!(
                    writes[1],
                    [
                        "tmux",
                        "paste-buffer",
                        "-p",
                        "-d",
                        "-b",
                        &writes[0][3],
                        "-t",
                        "$1:@5.%5"
                    ]
                );
                assert_eq!(writes[2], ["tmux", "send-keys", "-t", "$1:@5.%5", "Enter"]);
                assert!(spy.calls_named("select-window").is_empty());
                assert!(spy.calls_named("select-pane").is_empty());
                assert_eq!(*spy.room_queries.borrow(), [true; 3]);
                assert_eq!(spy.event_lists.get(), 1);
                assert_eq!(spy.assignment_events.get(), 1);
            }
            // An explicit --unbound conversation reports its absent task credit
            // on any Lead, although the alternate is outside typed binding.
            reset_alternate_assignment(&spy);
            let unbound = Options::parse(&commands::strings(&[
                "--orchestrator",
                "codex",
                "--lead",
                "alternate",
                "--account",
                "second",
                "--unbound",
                "--task",
                TASK,
            ]))
            .unwrap();
            let (_, output, _) =
                run_with_host(&spy.repo, "codex", &unbound, &spy, &spy.stop, &host).unwrap();
            let mut expected = alternate_result("submitted", Some("second".into()));
            expected["task_binding"] = "unbound".into();
            expected["task_completion"] = "unconfirmed".into();
            expected["acceptance"] = "unsupported".into();
            assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), expected);
        }

        #[test]
        fn codex_alternate_assignment_withholds_enter_on_changed_proof() {
            let (_tmp, spy, host, caller, home) = alternate_fixture();
            let ledger = write_synthetic_ledger(&spy.repo, 0);
            let events = spy.repo.join(".oms/lifecycle/events.jsonl");
            spy.real_events.set(true);
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            let assign = |caller: &AlternateCaller, alias: &str, task: &str| {
                spy.load()
                    .assign_codex_alternate(caller, alias, task, &host, &tx)
            };
            let early_refusal = |caller, alias, task: &str| {
                assert!(assign(caller, alias, task).is_err());
                assert!(spy.mutating_calls().is_empty());
            };
            let task = "synthetic literal task";
            early_refusal(&caller, "second", task);
            spy.load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap();
            reset_alternate_assignment(&spy);
            early_refusal(&caller, "first", task);
            early_refusal(&caller, "second", &"x".repeat(CODEX_VISIBLE_PASTE_CHARS));
            spy.room_successor.set(Some("room-successor"));
            early_refusal(&caller, "second", task);
            spy.room_successor.set(None);
            spy.leads.borrow_mut().push(FakeLead {
                window: "@6".into(),
                pane_id: "%6".into(),
                pid: 2003,
                active: false,
                provider: "codex".into(),
            });
            spy.alternate_parents
                .borrow_mut()
                .insert("%6".into(), ALTERNATE_LEAD_TAG.into());
            early_refusal(&caller, "second", task);
            spy.leads.borrow_mut().pop();
            spy.alternate_parents.borrow_mut().remove("%6");
            spy.alternate_task_mode.set(true);
            early_refusal(&caller, "second", task);
            spy.alternate_task_mode.set(false);
            spy.leads.borrow_mut()[1].active = true;
            early_refusal(&caller, "second", task);
            spy.leads.borrow_mut()[1].active = false;
            spy.screens
                .borrow_mut()
                .insert("%5".into(), format!("{CODEX_DRAFT}{CODEX_STATUS}"));
            early_refusal(&caller, "second", task);
            spy.screens.borrow_mut().insert(
                "%5".into(),
                format!("› \x1b[2mAn existing dim draft\x1b[0m{CODEX_STATUS}"),
            );
            early_refusal(&caller, "second", task);
            spy.screens.borrow_mut().insert(
                "%5".into(),
                format!("• Working (13m 53s • esc to interrupt)\n\n{CODEX_EMPTY}{CODEX_STATUS}"),
            );
            spy.alternate_task_cursor.set((2, 2));
            early_refusal(&caller, "second", task);

            let changed_after_paste = |change: Box<dyn FnOnce(&SpyPanel)>| {
                reset_alternate_assignment(&spy);
                spy.task_transition.replace(Some((
                    "paste-buffer",
                    Box::new(move |spy| {
                        spy.task_transition.replace(Some(("list-panes", change)));
                    }),
                )));
                assert!(
                    assign(&caller, "second", task)
                        .unwrap_err()
                        .publication_uncertain
                );
                assert_eq!(spy.calls_named("paste-buffer").len(), 1);
                assert!(spy.calls_named("send-keys").is_empty());
                assert_eq!(spy.event_lists.get(), 1);
            };
            changed_after_paste(Box::new(|spy| {
                spy.room_successor.set(Some("room-successor"));
            }));
            assert_eq!(*spy.room_queries.borrow(), [true; 3]);
            spy.room_successor.set(None);
            changed_after_paste(Box::new(|spy| {
                spy.task_transition.replace(Some((
                    "alternate-task-capture",
                    Box::new(|spy| spy.room_closed.set(true)),
                )));
            }));
            assert_eq!(*spy.room_queries.borrow(), [true; 3]);
            spy.room_closed.set(false);
            changed_after_paste(Box::new(|spy| {
                let mut screens = spy.screens.borrow_mut();
                let screen = screens.get_mut("%5").unwrap();
                *screen = screen.replace("weekly 48%", "weekly 49%");
            }));
            changed_after_paste(Box::new(|spy| {
                let mut screens = spy.screens.borrow_mut();
                let screen = screens.get_mut("%5").unwrap();
                *screen = format!("Earlier output arrived while pasting\n{screen}");
                let (x, y) = spy.alternate_task_cursor.get();
                spy.alternate_task_cursor.set((x, y + 1));
            }));
            changed_after_paste(Box::new(|spy| {
                append_lifecycle_row(&spy.repo, &json!({"unrelated": "ledger append"}));
            }));
            std::fs::write(&events, &ledger).unwrap();
            changed_after_paste(Box::new(|spy| {
                spy.leads.borrow_mut()[1].pid = 2003;
            }));
            spy.leads.borrow_mut()[1].pid = 2002;
            changed_after_paste(Box::new(|spy| spy.alternate_task_command.set("sh")));
            spy.alternate_task_command.set("codex");
            changed_after_paste(Box::new(|spy| spy.alternate_locally_marked.set(false)));
            spy.alternate_locally_marked.set(true);
            let old = home.with_extension("retired");
            let (away, replaced) = (old.clone(), home.clone());
            changed_after_paste(Box::new(move |_| {
                std::fs::rename(&replaced, &away).unwrap();
                private_dir(&replaced);
            }));
            std::fs::remove_dir(&home).unwrap();
            std::fs::rename(old, home).unwrap();
        }

        #[test]
        fn codex_alternate_launch_focus_and_default_pane_are_distinct() {
            let (_tmp, spy, host, caller, home) = alternate_fixture();
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            let set_mode = |mode| {
                spy.event_projection.borrow_mut().as_mut().unwrap()[0]["refs"]
                    [MAIN_PERMISSIONS_REF] = json!(mode);
            };
            set_mode("unknown");
            let error = spy
                .load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap_err();
            assert_eq!(
                error.message,
                "OMS_PANEL_MAIN_PERMISSIONS must be full or native"
            );
            assert!(spy.mutating_calls().is_empty());
            set_mode("native");
            let output = spy
                .load()
                .launch_codex_alternate(
                    &caller,
                    "second",
                    LeadLaunch {
                        model: Some("vendor/model-x"),
                        effort: Some("high"),
                        ..Default::default()
                    },
                    &host,
                    &tx,
                )
                .unwrap();
            assert_eq!(output, alternate_result("started", Some("second".into())));
            let spawn = spy.calls_named("new-window");
            let spawn = &spawn[0];
            assert!(
                spawn
                    .windows(2)
                    .any(|args| args == ["-F", ALTERNATE_SPAWN_FORMAT])
            );
            assert!(spawn.windows(2).any(|args| args == ["-t", "$1:"]));
            for scrub in [
                "CODEX_HOME",
                "OPENAI_BASE_URL",
                "OMS_AGENT",
                "TMUX",
                "TMUX_PANE",
            ] {
                assert!(spawn.windows(2).any(|args| args == ["-u", scrub]));
            }
            assert!(spawn.contains(&format!("CODEX_HOME={}", home.display())));
            assert_eq!(
                spawn[spawn.len() - 5..],
                [
                    "codex",
                    "--model",
                    "vendor/model-x",
                    "-c",
                    "model_reasoning_effort=\"high\"",
                ]
            );
            assert!(spawn.contains(&"OMS_HARNESS_CHILD=1".to_owned()));
            assert!(spawn.contains(&"OMS_HARNESS_DELEGATE_DEPTH=1".to_owned()));
            assert!(spy.calls_named("rename-window").is_empty());
            assert_eq!(
                spy.calls_named("set-option"),
                [[
                    "tmux",
                    "set-option",
                    "-p",
                    "-t",
                    "$1:@5.%5",
                    "@oms_provider_parent",
                    ALTERNATE_LEAD_TAG,
                    ";",
                    "set-option",
                    "-w",
                    "-t",
                    "$1:@5.%5",
                    "@oms_provider_session",
                    "codex",
                ]]
            );
            assert_eq!(spy.load().connected(), json!(["codex"]));

            let bind_dir = host
                .state_home
                .join("oh-my-setting/panel-control")
                .join(account_binding::path_digest(&spy.repo))
                .join("account-bindings");
            let receipt = std::fs::read_dir(bind_dir)
                .unwrap()
                .map(|f| f.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("orchestrator-alternate-")
                })
                .unwrap();
            assert_eq!(
                std::fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let body = std::fs::read_to_string(receipt).unwrap();
            assert!(!body.contains(home.to_str().unwrap()));
            let binding: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(binding["alternate_lead"]["window"], "@5");
            assert_eq!(binding["alternate_lead"]["main_pane"], "%1");
            assert_eq!(binding["alternate_lead"]["main_birth"], "main");
            assert_eq!(binding["room"], ROOM);
            assert_eq!(binding["main_attempt"], MAIN);
            assert_eq!(binding["session"]["session"], "$1");
            assert_eq!(binding["pane_id"], "%5");
            assert_eq!(binding["pid"], 2002);
            assert_eq!(binding["birth"], "alternate");

            spy.event_projection.replace(None);
            spy.event_lists.set(0);
            spy.room_queries.borrow_mut().clear();
            spy.calls.borrow_mut().clear();
            assert_eq!(
                spy.load()
                    .focus_codex_alternate(&caller, &host, &tx)
                    .unwrap(),
                alternate_result("focused", Some("second".into()))
            );
            assert_eq!(*spy.room_queries.borrow(), [true]);
            assert_eq!(spy.event_lists.get(), 0);
            assert_eq!(
                spy.calls
                    .borrow()
                    .iter()
                    .map(|call| call[1].as_str())
                    .collect::<Vec<_>>(),
                [
                    "list-panes",
                    "display-message",
                    "list-panes",
                    "display-message",
                    "display-message",
                    "select-pane"
                ]
            );
            assert_eq!(
                spy.calls_named("select-pane"),
                [[
                    "tmux",
                    "select-pane",
                    "-t",
                    "$1:@5.%5",
                    ";",
                    "select-window",
                    "-t",
                    "$1:@5"
                ]]
            );
            spy.calls.borrow_mut().clear();
            spy.load().focus("codex").unwrap();
            assert_eq!(
                spy.calls_named("select-window"),
                [["tmux", "select-window", "-t", "$1:@4"]]
            );
            assert_eq!(
                spy.calls_named("select-pane"),
                [["tmux", "select-pane", "-t", "%4"]]
            );

            spy.leads.borrow_mut().retain(|lead| lead.pane_id != "%4");
            spy.calls.borrow_mut().clear();
            for account in [Some("second"), None] {
                assert!(
                    spy.load()
                        .execute_with_host("codex", account, None, &host)
                        .is_err()
                );
            }
            assert!(spy.calls_named("new-window").is_empty());
            spy.spawn_pid.set(2003);
            let primary = spy
                .load()
                .execute_with_host("codex", Some("first"), None, &host)
                .unwrap();
            assert_eq!(primary["status"], "started");
            assert!(primary.get("lead").is_none());
            assert_eq!(spy.load().leads["codex"].id, "%6");

            spy.leads.borrow_mut().retain(|lead| lead.pane_id != "%6");
            spy.leads.borrow_mut()[0].active = true;
            spy.calls.borrow_mut().clear();
            for provider in ["", "invalid"] {
                spy.leads.borrow_mut()[0].provider = provider.into();
                let error = spy
                    .load()
                    .execute_with_host("codex", Some("first"), None, &host)
                    .unwrap_err();
                assert_eq!(
                    error.message,
                    "provider already has a live pane in this panel"
                );
            }
            assert!(spy.calls_named("new-window").is_empty());

            spy.dead_panes.borrow_mut().insert("%5".into());
            let primary = spy
                .load()
                .execute_with_host("codex", Some("second"), None, &host)
                .unwrap();
            assert_eq!(primary["status"], "started");
            assert_eq!(primary["account"], "second");
            assert_eq!(spy.load().leads["codex"].id, "%7");
        }

        #[test]
        fn codex_alternate_refuses_conflicting_scope_accounts_and_generations() {
            let (tmp, spy, mut host, mut caller, home) = alternate_fixture();
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            let refuse_launch = |caller: &AlternateCaller, host: &FakeHost, alias| {
                spy.calls.borrow_mut().clear();
                assert!(
                    spy.load()
                        .launch_codex_alternate(caller, alias, LeadLaunch::default(), host, &tx)
                        .is_err()
                );
                assert!(spy.mutating_calls().is_empty());
            };
            refuse_launch(&caller, &host, "first");
            refuse_launch(&caller, &host, "unknown");
            caller.campaign = true;
            refuse_launch(&caller, &host, "second");
            caller.campaign = false;
            caller.owner = "claude".into();
            refuse_launch(&caller, &host, "second");
            caller.owner = "codex".into();
            spy.codex_chat_marks.borrow_mut().remove("%4");
            refuse_launch(&caller, &host, "second");
            spy.codex_chat_marks
                .borrow_mut()
                .insert("%4".into(), "1".into());
            host.env_set.insert("OPENAI_BASE_URL".into());
            refuse_launch(&caller, &host, "second");
            host.env_set.clear();
            spy.room_successor.set(Some("new-room"));
            refuse_launch(&caller, &host, "second");
            spy.room_successor.set(None);

            spy.load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap();
            refuse_launch(&caller, &host, "second");
            let refuse_focus = |host: &FakeHost| {
                spy.calls.borrow_mut().clear();
                assert!(
                    spy.load()
                        .focus_codex_alternate(&caller, host, &tx)
                        .is_err()
                );
                assert!(spy.mutating_calls().is_empty());
                assert!(spy.calls_named("select-pane").is_empty());
                assert!(spy.calls_named("select-window").is_empty());
            };
            spy.alternate_locally_marked.set(false);
            refuse_focus(&host);
            spy.alternate_locally_marked.set(true);
            spy.room_closed.set(true);
            refuse_focus(&host);
            spy.room_closed.set(false);
            for pid in [spy.main_pid, 2001, 2002] {
                host.reused_pid.set(Some(pid));
                refuse_focus(&host);
            }
            host.reused_pid.set(None);
            let moved_home = tmp.0.join("moved-home");
            std::fs::rename(&home, &moved_home).unwrap();
            private_dir(&home);
            refuse_focus(&host);
            std::fs::remove_dir(&home).unwrap();
            std::fs::rename(&moved_home, &home).unwrap();
            spy.leads.borrow_mut()[1].window = "@8".into();
            refuse_focus(&host);
            spy.leads.borrow_mut()[1].window = "@5".into();
            spy.leads.borrow_mut()[1].provider = "claude".into();
            refuse_focus(&host);
            spy.leads.borrow_mut()[1].provider = "codex".into();
            spy.leads.borrow_mut().push(FakeLead {
                window: "@9".into(),
                pane_id: "%9".into(),
                pid: 2003,
                active: false,
                provider: "codex".into(),
            });
            spy.alternate_parents
                .borrow_mut()
                .insert("%9".into(), ALTERNATE_LEAD_TAG.into());
            refuse_focus(&host);
            spy.leads.borrow_mut().pop();
            spy.alternate_parents.borrow_mut().remove("%5");
            spy.leads.borrow_mut()[1].provider.clear();
            refuse_focus(&host);
        }

        #[test]
        fn codex_alternate_launch_rolls_back_only_the_new_pane_generation() {
            let (_tmp, spy, host, caller, _) = alternate_fixture();
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            spy.fail_parent_tag.set(true);
            assert!(
                spy.load()
                    .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                    .is_err()
            );
            assert_eq!(
                spy.calls_named("kill-pane"),
                [["tmux", "kill-pane", "-t", "$1:@5.%5"]]
            );
            assert_eq!(spy.load().leads["codex"].id, "%4");

            spy.calls.borrow_mut().clear();
            spy.fail_parent_tag.set(false);
            spy.task_transition.replace(Some((
                "new-window",
                Box::new(|spy| {
                    spy.leads.borrow_mut().last_mut().unwrap().window = "@99".into();
                }),
            )));
            let error = spy
                .load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap_err();
            assert!(error.message.contains("could not be safely rolled back"));
            assert!(error.message.contains("may still exist"));
            assert!(error.publication_uncertain);
            assert!(spy.calls_named("set-option").is_empty());
            assert!(spy.calls_named("kill-pane").is_empty());
        }

        #[test]
        fn codex_alternate_reopens_with_a_retained_dead_pane() {
            let (_tmp, spy, host, caller, _) = alternate_fixture();
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            spy.load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap();

            spy.leads.borrow_mut()[1].active = true;
            spy.dead_panes.borrow_mut().insert("%5".into());
            spy.calls.borrow_mut().clear();
            let error = spy
                .load()
                .focus_codex_alternate(&caller, &host, &tx)
                .unwrap_err();
            assert!(error.message.contains("not connected"), "{error}");
            assert!(spy.calls_named("select-pane").is_empty());

            spy.spawn_pid.set(2003);
            spy.load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap();
            spy.load()
                .focus_codex_alternate(&caller, &host, &tx)
                .unwrap();
            assert_eq!(&spy.calls_named("select-pane")[0][3], "$1:@6.%6");

            spy.leads.borrow_mut()[1].window = "@6".into();
            spy.calls.borrow_mut().clear();
            assert!(
                spy.load()
                    .focus_codex_alternate(&caller, &host, &tx)
                    .is_err()
            );
            assert!(spy.calls_named("select-pane").is_empty());
            assert!(spy.calls_named("kill-pane").is_empty());
        }

        #[test]
        fn codex_alternate_unverified_spawn_preserves_potential_new_pane() {
            let (_tmp, spy, host, caller, _) = alternate_fixture();
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            spy.leads.borrow_mut().push(FakeLead {
                window: "@9".into(),
                pane_id: "%9".into(),
                pid: 2003,
                active: false,
                provider: String::new(),
            });
            spy.dead_panes.borrow_mut().insert("%9".into());
            for reply in [
                out("malformed"),
                out("%5\tnot-a-pid\t@5\t$1"),
                out("%5\t+2002\t@5\t$1"),
                out("%5\t2001\t@5\t$1"),
                out("%5\t2004\t@5\t$1"),
                out("%5\t2002\t@5\t$2"),
                out("%9\t2002\t@5\t$1"),
                out("%5\t2002\t@9\t$1"),
                CommandOutput {
                    output_complete: false,
                    ..out("")
                },
            ] {
                spy.alternate_spawn_reply.replace(Some(reply));
                spy.calls.borrow_mut().clear();
                let error = spy
                    .load()
                    .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                    .unwrap_err();
                assert!(
                    error.message.contains("pane may have been created"),
                    "{error}"
                );
                assert!(error.publication_uncertain);
                assert_eq!(spy.calls_named("new-window").len(), 1);
                assert!(spy.calls_named("kill-pane").is_empty());
                assert!(spy.calls_named("set-option").is_empty());
                let mut leads = spy.leads.borrow_mut();
                assert_eq!(leads.len(), 3);
                assert_eq!(leads.pop().unwrap().provider, "");
                spy.next_pane.set(5);
                spy.next_window.set(5);
            }

            spy.leads.borrow_mut()[1].pane_id = "invalid".into();
            spy.calls.borrow_mut().clear();
            let panel = spy.load();
            assert_eq!(panel.connected(), json!(["codex"]));
            let error = panel
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &host, &tx)
                .unwrap_err();
            assert!(error.message.contains("pane census"), "{error}");
            assert!(!error.publication_uncertain);
            assert!(spy.calls_named("new-window").is_empty());
        }

        #[test]
        fn provider_tags_use_one_ordered_tmux_sequence() {
            let tmp = TempDir::new("orch-provider-tags");
            let spy = SpyPanel::new(private_dir(&tmp.0.join("repo")));
            let panel = spy.load();
            for (scope, pane, provider) in [("-w", "%4", "codex"), ("-p", "%5", "claude")] {
                spy.calls.borrow_mut().clear();
                panel.tag_provider(pane, provider, scope).unwrap();
                assert_eq!(
                    spy.calls_named("set-option"),
                    [[
                        "tmux",
                        "set-option",
                        scope,
                        "-t",
                        pane,
                        "@oms_panel_room",
                        ROOM,
                        ";",
                        "set-option",
                        scope,
                        "-t",
                        pane,
                        "@oms_provider_parent",
                        MAIN,
                        ";",
                        "set-option",
                        scope,
                        "-t",
                        pane,
                        "@oms_provider_session",
                        provider,
                    ]]
                );
            }
        }

        #[test]
        fn prelaunch_refusals_and_post_spawn_cleanup() {
            let tmp = TempDir::new("orch-prelaunch");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-a"));
            let codex_bad_mode = tmp.0.join("codex-bad-mode");
            std::fs::create_dir_all(&codex_bad_mode).unwrap();
            std::fs::set_permissions(&codex_bad_mode, std::fs::Permissions::from_mode(0o775))
                .unwrap();
            let claude_a = private_dir(&tmp.0.join("claude-a"));
            let uid = rustix::process::getuid().as_raw();

            std::fs::write(
                codex_a.join("models_cache.json"),
                br#"{"models":[{"slug":"gpt-6.1-sol"}]}"#,
            )
            .unwrap();

            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![
                (
                    "codex",
                    vec![("acct-a", codex_a.clone()), ("bad-mode", codex_bad_mode)],
                ),
                ("claude", vec![("acct-a", claude_a)]),
            ]));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));

            let spy = SpyPanel::new(repo);

            // Unknown alias, unregistered default, bad mode, missing model in models_cache, antigravity --account:
            for (provider, acct) in [
                ("codex", "unknown"),
                ("codex", "default"),
                ("codex", "bad-mode"),
                ("codex", "acct-a"), // models_cache only has gpt-6.1-sol
                ("antigravity", "acct-a"),
            ] {
                assert!(
                    spy.load()
                        .execute_with_host(provider, Some(acct), None, &host)
                        .is_err(),
                    "{provider}/{acct}"
                );
                assert!(spy.mutating_calls().is_empty(), "{provider}/{acct}");
            }

            // Invalid --account takes priority over "open the provider session before sending a task".
            let err = spy
                .load()
                .execute_with_host("codex", Some("unknown"), Some("do work"), &host)
                .unwrap_err();
            assert!(!err.message.contains("open the provider session"));
            assert!(spy.mutating_calls().is_empty());

            // Valid --account with --task when no pane is live keeps the two-step error.
            let two_step = spy
                .load()
                .execute_with_host("claude", Some("acct-a"), Some("do work"), &host)
                .unwrap_err();
            assert_eq!(
                two_step.message,
                "open the provider session before sending a task"
            );
            assert!(spy.mutating_calls().is_empty());

            // Claude ANTHROPIC_API_KEY in host env or tmux env blocks launch before new-window.
            let cred_key = "ANTHROPIC_API_KEY";
            host.env_set.insert(cred_key.into());
            assert!(
                spy.load()
                    .execute_with_host("claude", Some("acct-a"), None, &host)
                    .is_err()
            );
            assert!(spy.mutating_calls().is_empty());
            host.env_set.clear();

            spy.session_env
                .borrow_mut()
                .push(format!("{cred_key}=fixture-present"));
            assert!(
                spy.load()
                    .execute_with_host("claude", Some("acct-a"), None, &host)
                    .is_err()
            );
            assert!(spy.mutating_calls().is_empty());
            spy.session_env.borrow_mut().clear();

            // A failed parent tag prevents provider admission and cleans up the new pane.
            std::fs::write(
                codex_a.join("models_cache.json"),
                br#"{"models":[{"slug":"gpt-6.2-sol"}]}"#,
            )
            .unwrap();
            spy.fail_parent_tag.set(true);
            assert!(
                spy.load()
                    .execute_with_host("codex", Some("acct-a"), None, &host)
                    .is_err()
            );
            assert_eq!(*spy.provider_tag_writes.borrow(), ["@oms_panel_room"]);
            let mutations = spy.mutating_calls();
            assert_eq!(
                mutations
                    .iter()
                    .map(|args| args[1].as_str())
                    .collect::<Vec<_>>(),
                ["new-window", "set-option", "set-option", "kill-pane"]
            );
            assert_eq!(mutations[1][5], "@oms_codex_chat_bound");
            assert_eq!(mutations[2][5], "@oms_panel_room");
            assert!(spy.calls_named("rename-window").is_empty());
            assert!(spy.leads.borrow().is_empty());
        }

        fn write_synthetic_ledger(repo: &Path, extra_attempts: usize) -> String {
            use std::fmt::Write as _;
            let lifecycle = repo.join(".oms/lifecycle");
            std::fs::create_dir_all(&lifecycle).unwrap();
            // Room storage refuses a group- or other-writable `.oms`; the ambient umask must not decide that.
            for dir in [repo.join(".oms"), lifecycle.clone()] {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut out = String::with_capacity(extra_attempts * 220 + 1024);
            for idx in 0..extra_attempts {
                writeln!(
                    out,
                    r#"{{"schema":1,"event_id":"aevt_a00000000000000000000000{idx:08x}","ts":"2026-11-19T00:00:00Z","attempt_id":"att_a00000000000000000000000{idx:08x}","seq":1,"event_type":"attempt.created","provider":"codex","tool":"peer-ask","to_state":"queued","actor":{{"kind":"owner","name":"agent-events"}}}}"#
                )
                .unwrap();
            }
            writeln!(
                out,
                r#"{{"schema":1,"event_id":"aevt_11111111111111111111111100000001","ts":"2026-11-19T00:00:00Z","attempt_id":"{MAIN}","seq":1,"event_type":"attempt.created","provider":"codex","tool":"panel-main","to_state":"queued","actor":{{"kind":"owner","name":"agent-events"}},"refs":{{"panel_role":"main","panel_room_id":"{ROOM}","panel_native_scheduling":"unmanaged"}}}}"#
            )
            .unwrap();
            writeln!(
                out,
                r#"{{"schema":1,"event_id":"aevt_11111111111111111111111100000002","ts":"2026-11-19T00:00:01Z","attempt_id":"{MAIN}","seq":2,"event_type":"attempt.state_changed","from_state":"queued","to_state":"starting","actor":{{"kind":"owner","name":"agent-events"}}}}"#
            )
            .unwrap();
            writeln!(
                out,
                r#"{{"schema":1,"event_id":"aevt_11111111111111111111111100000003","ts":"2026-11-19T00:00:02Z","attempt_id":"{MAIN}","seq":3,"event_type":"attempt.state_changed","from_state":"starting","to_state":"working","actor":{{"kind":"owner","name":"agent-events"}}}}"#
            )
            .unwrap();
            std::fs::write(lifecycle.join("events.jsonl"), &out).unwrap();
            out
        }

        fn append_lifecycle_row(repo: &Path, row: &Value) {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(repo.join(".oms/lifecycle/events.jsonl"))
                .unwrap();
            writeln!(file, "{row}").unwrap();
        }

        fn main_failed_row(seq: u64, tag: u32) -> Value {
            json!({
                "schema": 1,
                "event_id": format!("aevt_c00000000000000000000000{tag:08x}"),
                "ts": "2026-11-19T00:00:05Z",
                "attempt_id": MAIN,
                "seq": seq,
                "event_type": "attempt.state_changed",
                "from_state": "working",
                "to_state": "failed",
                "actor": {"kind": "owner", "name": "agent-events"}
            })
        }

        fn benign_heartbeat_row(attempt_id: &str, seq: u64, tag: u32) -> Value {
            json!({
                "schema": 1,
                "event_id": format!("aevt_b00000000000000000000000{tag:08x}"),
                "ts": "2026-11-19T00:00:04Z",
                "attempt_id": attempt_id,
                "seq": seq,
                "event_type": "attempt.heartbeat",
                "actor": {"kind": "runner", "name": "attempt-runner"}
            })
        }

        #[test]
        fn already_live_mismatched_and_untagged_panes_refuse_rebind_and_preserve_draft() {
            let tmp = TempDir::new("orch-rebind");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-a"));
            let codex_b = private_dir(&tmp.0.join("codex-b"));
            let models = codex_a.join("models_cache.json");
            std::fs::write(&models, br#"{"models":[{"slug":"vendor/model-x"}]}"#).unwrap();
            std::fs::set_permissions(&models, std::fs::Permissions::from_mode(0o600)).unwrap();
            let uid = rustix::process::getuid().as_raw();

            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_a.clone()), ("acct-b", codex_b.clone())],
            )]));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));
            host.owners.insert(2002, owner_entry(2002, "p2002", uid));

            let spy = SpyPanel::new(repo);
            spy.allow_board_queries.set(true);
            spy.real_events.set(true);
            let base_ledger = write_synthetic_ledger(&spy.repo, 0);
            let events_path = spy.repo.join(".oms/lifecycle/events.jsonl");
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "fixture-main", uid));
            spy.global_env
                .borrow_mut()
                .push(format!("OMS_ROOM_PARTICIPANT={MAIN}"));
            // Open the pane without submitting. Strip the parent's participant from launch.
            let requested = LeadLaunch {
                model: Some("vendor/model-x"),
                effort: Some("xhigh"),
                permissions: Some(MainPermissions::Full),
            };
            spy.load()
                .execute_with_host_cached(
                    "codex",
                    Some("acct-a"),
                    None,
                    requested,
                    LeadExecution {
                        host: &host,
                        attempts: &AttemptCache::default(),
                        transaction: None,
                    },
                )
                .unwrap();
            let launch = &spy.calls_named("new-window")[0];
            assert!(launch.ends_with(&[
                "codex".into(),
                "--model".into(),
                "vendor/model-x".into(),
                "--dangerously-bypass-approvals-and-sandbox".into(),
                "-c".into(),
                "model_reasoning_effort=\"xhigh\"".into(),
            ]));
            assert!(
                launch
                    .windows(2)
                    .any(|args| args == ["-u", "OMS_ROOM_PARTICIPANT"])
            );
            assert!(
                !launch
                    .iter()
                    .any(|arg| arg.contains(MAIN) || arg == STARTUP_PROMPT)
            );
            assert!(spy.calls_named("paste-buffer").is_empty());
            assert!(spy.calls_named("send-keys").is_empty());
            spy.calls.borrow_mut().clear();
            assert_eq!(
                spy.requested_model_marks
                    .borrow()
                    .get("%4")
                    .map(String::as_str),
                Some("vendor/model-x"),
            );
            let explicit_reuse = spy
                .load()
                .execute_with_host_cached(
                    "codex",
                    Some("acct-a"),
                    None,
                    requested,
                    LeadExecution {
                        host: &host,
                        attempts: &AttemptCache::default(),
                        transaction: None,
                    },
                )
                .unwrap_err();
            assert!(explicit_reuse.message.contains("already connected"));
            assert!(spy.mutating_calls().is_empty());

            // Put a simulated user draft in the live pane screen; mismatch must NOT touch tmux.
            spy.screens
                .borrow_mut()
                .insert("%4".into(), "› user draft preserved".into());

            // Mismatched --account acct-b refused cleanly with zero mutating calls.
            for task in [None, Some("work on bug")] {
                assert!(
                    spy.load()
                        .execute_with_host("codex", Some("acct-b"), task, &host)
                        .is_err()
                );
                assert!(spy.mutating_calls().is_empty());
            }

            // Matching --account acct-a succeeds ("connected") and submits --task with ALTERNATE_STARTUP_PROMPT.
            let connected = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();
            assert_eq!(connected["status"], "connected");
            assert_eq!(connected["account"], "acct-a");
            assert_eq!(connected["account_pinned"], true);
            assert_eq!(connected["account_verification"], "verified");
            spy.requested_model_marks
                .borrow_mut()
                .insert("%4".into(), "invalid model".into());
            assert!(
                spy.load()
                    .execute_with_host("codex", Some("acct-a"), None, &host)
                    .unwrap_err()
                    .message
                    .contains("model marker is invalid")
            );
            assert!(spy.mutating_calls().is_empty());
            spy.requested_model_marks
                .borrow_mut()
                .insert("%4".into(), "vendor/model-x".into());

            let send = |requested, host: &FakeHost| {
                spy.calls.borrow_mut().clear();
                spy.room_queries.borrow_mut().clear();
                spy.event_lists.set(0);
                spy.assignment_events.set(0);
                spy.cursor_x.set(2);
                let status = CODEX_DISPLAY_STATUS.replace("GPT-6.2-Sol", "vendor/model-x");
                spy.screens
                    .borrow_mut()
                    .insert("%4".into(), format!("{CODEX_EMPTY}{status}"));
                spy.load().execute_with_host(
                    "codex",
                    requested,
                    Some(r#"check {"kind":"read"}; literal"#),
                    host,
                )
            };
            let submitted = send(Some("acct-a"), &host).unwrap();
            assert_eq!(submitted["status"], "submitted");
            assert_eq!(submitted["account"], "acct-a");
            assert_eq!(submitted["account_pinned"], true);
            assert_eq!(submitted["account_verification"], "verified");
            let buffers = spy.calls_named("set-buffer");
            assert_eq!(buffers.len(), 1);
            assert!(buffers[0][5].contains(". You own this provider's work. "));
            assert!(buffers[0][5].contains(ALTERNATE_STARTUP_PROMPT));
            assert!(!buffers[0][5].contains(STARTUP_PROMPT.trim()));
            assert!(buffers[0][5].chars().count() <= CODEX_VISIBLE_PASTE_CHARS);
            assert!(buffers[0][5].contains(&format!(
                "oms room show --repo '{}' --id '{ROOM}' --exact --json",
                spy.repo.display()
            )));
            assert!(buffers[0][5].ends_with(r#"Task: check {"kind":"read"}; literal"#));
            assert!(!buffers[0][5].contains(MAIN));
            assert!(!buffers[0][5].contains(codex_a.to_str().unwrap()));
            let turn = buffers[0][5]
                .split_once(" Turn ")
                .unwrap()
                .1
                .split_once(". ")
                .unwrap()
                .0;
            assert_eq!(turn.len(), 20);
            assert!(
                turn.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert_eq!(*spy.room_queries.borrow(), [true; 3]);
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.assignment_events.get(), 1);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert_eq!(
                spy.calls_named("send-keys"),
                [["tmux", "send-keys", "-t", "%4", "Enter"]]
            );

            // Rechecks observe changes after the initial receipt, before any buffer or pane write.
            spy.task_transition.replace(Some((
                "capture-pane",
                Box::new(|spy| {
                    spy.room_successor.set(Some("room-successor"));
                }),
            )));
            assert!(send(None, &host).is_err());
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(*spy.room_queries.borrow(), [true]);
            assert_eq!(spy.event_lists.get(), 1);
            spy.room_successor.set(None);

            let host = std::rc::Rc::new(host);
            for pid in [spy.main_pid, 2001] {
                let changed = host.clone();
                spy.task_transition.replace(Some((
                    "capture-pane",
                    Box::new(move |_| {
                        changed.reused_pid.set(Some(pid));
                    }),
                )));
                assert!(send(None, &host).is_err());
                assert!(spy.mutating_calls().is_empty());
                host.reused_pid.set(None);
            }

            let mut remapped = (*host).clone();
            remapped.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_b.clone())],
            )]));
            assert!(send(None, &remapped).is_err());
            assert!(spy.mutating_calls().is_empty());

            // A same-path directory replacement between the first proof and paste also fails.
            let renamed = tmp.0.join("retired-home");
            let move_from = codex_a.clone();
            let move_to = renamed.clone();
            spy.task_transition.replace(Some((
                "capture-pane",
                Box::new(move |_| {
                    std::fs::rename(&move_from, &move_to).unwrap();
                    private_dir(&move_from);
                }),
            )));
            assert!(send(None, &host).is_err());
            assert!(spy.mutating_calls().is_empty());
            std::fs::remove_dir(&codex_a).unwrap();
            std::fs::rename(renamed, &codex_a).unwrap();

            // Pre-set-buffer Main terminalization fails closed with zero mutating tmux calls.
            spy.task_transition.replace(Some((
                "capture-pane",
                Box::new(|spy| {
                    append_lifecycle_row(&spy.repo, &main_failed_row(4, 1));
                }),
            )));
            let pre_buffer_stale = send(None, &host).unwrap_err();
            assert!(pre_buffer_stale.message.contains("live Main attempt"));
            assert!(!pre_buffer_stale.publication_uncertain);
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(spy.event_lists.get(), 1);
            std::fs::write(&events_path, &base_ledger).unwrap();

            // Post-set-buffer Main terminalization triggers a refresh, deletes the buffer, and aborts before paste.
            spy.task_transition.replace(Some((
                "set-buffer",
                Box::new(|spy| {
                    append_lifecycle_row(&spy.repo, &main_failed_row(4, 2));
                }),
            )));
            let stale = send(None, &host).unwrap_err();
            assert!(stale.message.contains("live Main attempt"));
            assert!(!stale.publication_uncertain);
            assert_eq!(spy.event_lists.get(), 2);
            assert_eq!(spy.assignment_events.get(), 2);
            assert_eq!(spy.calls_named("delete-buffer").len(), 1);
            assert!(spy.calls_named("paste-buffer").is_empty());
            assert!(spy.calls_named("send-keys").is_empty());
            std::fs::write(&events_path, &base_ledger).unwrap();

            // A change after the paste refuses Enter and reports a possible remaining draft.
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(|spy| {
                    spy.room_successor.set(Some("room-successor"));
                }),
            )));
            let uncertain = send(None, &host).unwrap_err();
            assert!(uncertain.publication_uncertain && uncertain.message.contains("draft"));
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            spy.room_successor.set(None);

            let replaced = host.clone();
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(move |_| replaced.reused_pid.set(Some(2001))),
            )));
            assert!(send(None, &host).unwrap_err().publication_uncertain);
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            host.reused_pid.set(None);

            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(|spy| {
                    append_lifecycle_row(&spy.repo, &main_failed_row(4, 3));
                }),
            )));
            let post_paste_main = send(None, &host).unwrap_err();
            assert!(post_paste_main.publication_uncertain);
            assert!(post_paste_main.message.contains("live Main attempt"));
            assert_eq!(spy.event_lists.get(), 2);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            std::fs::write(&events_path, &base_ledger).unwrap();

            // The mark denies missing custody, but a genuine pre-binding pane can send.
            let mut host = (*host).clone();
            host.state_home = private_dir(&tmp.0.join("unbound-state"));
            assert!(send(None, &host).is_err());
            assert!(spy.mutating_calls().is_empty());
            spy.codex_chat_marks.borrow_mut().clear();
            spy.leads.borrow_mut()[0].pid = 2002;
            let unbound = send(None, &host).unwrap();
            assert_eq!(unbound["account_verification"], "unverified");
            assert!(unbound["account"].is_null());
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert_eq!(spy.calls_named("send-keys").len(), 1);

            // Without .oms/lifecycle/events.jsonl, mock event_projection still works and stays uncached.
            std::fs::remove_file(&events_path).unwrap();
            spy.real_events.set(false);
            spy.event_projection.replace(Some(json!([{
                "attempt_id": MAIN,
                "provider": "codex",
                "tool": "panel-main",
                "terminal": false,
                "state": "working",
                "refs": {
                    "panel_role": "main",
                    "panel_room_id": ROOM,
                    "panel_native_scheduling": "unmanaged"
                }
            }])));
            assert!(send(None, &host).is_ok());
            assert_eq!(spy.event_lists.get(), 3);
            spy.event_projection.replace(None);

            // Add an untagged claude pane whose ambient active_home happens to point to acct-a:
            // must NEVER retroactively bind ambient env!
            let claude_a = private_dir(&tmp.0.join("claude-a"));
            host.accounts = Some(make_accounts(vec![
                (
                    "codex",
                    vec![("acct-a", codex_a), ("acct-b", tmp.0.join("codex-b"))],
                ),
                ("claude", vec![("acct-a", claude_a.clone())]),
            ]));
            host.active_homes.insert("claude".into(), claude_a);
            spy.leads.borrow_mut().push(FakeLead {
                window: "@9".into(),
                pane_id: "%9".into(),
                pid: 2002,
                active: false,
                provider: "claude".into(),
            });
            spy.calls.borrow_mut().clear();

            for task in [None, Some("task draft")] {
                assert!(
                    spy.load()
                        .execute_with_host("claude", Some("acct-a"), task, &host)
                        .is_err()
                );
                assert!(spy.mutating_calls().is_empty());
            }
            // Without --account, untagged pane connects as Unverified (no retroactive binding).
            let untagged_conn = spy
                .load()
                .execute_with_host("claude", None, None, &host)
                .unwrap();
            assert_eq!(untagged_conn["status"], "connected");
            assert_eq!(untagged_conn["account"], Value::Null);
            assert_eq!(untagged_conn["account_pinned"], false);
            assert_eq!(untagged_conn["account_verification"], "unverified");
        }

        fn primary_codex_fixture(tag: &str) -> (TempDir, SpyPanel, FakeHost, PathBuf) {
            let tmp = TempDir::new(tag);
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-a"));
            let uid = rustix::process::getuid().as_raw();
            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_a.clone())],
            )]));
            let spy = SpyPanel::new(repo);
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "fixture-main", uid));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));
            spy.allow_board_queries.set(true);
            spy.real_events.set(true);
            write_synthetic_ledger(&spy.repo, 0);
            spy.load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();
            spy.calls.borrow_mut().clear();
            spy.room_queries.borrow_mut().clear();
            spy.event_lists.set(0);
            spy.assignment_events.set(0);
            spy.cursor_x.set(2);
            spy.screens
                .borrow_mut()
                .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            (tmp, spy, host, codex_a)
        }

        #[test]
        fn codex_primary_task_uses_alternate_prompt_under_cap_and_preserves_guards() {
            let (_tmp, spy, host, codex_a) = primary_codex_fixture("orch-primary-codex-short");
            let task = "inspect crate";
            let submitted = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some(task), &host)
                .unwrap();
            assert_eq!(submitted["status"], "submitted");
            assert_eq!(submitted["account"], "acct-a");
            assert_eq!(submitted["account_pinned"], true);
            assert_eq!(submitted["account_verification"], "verified");
            let buffers = spy.calls_named("set-buffer");
            assert_eq!(buffers.len(), 1);
            let prompt = &buffers[0][5];
            let locator = assignment_locator(&spy.repo, ROOM).unwrap();
            let turn = prompt
                .split_once(" Turn ")
                .unwrap()
                .1
                .split_once(". ")
                .unwrap()
                .0;
            assert_eq!(turn.len(), 20);
            assert_eq!(
                prompt,
                &format!(
                    "Task from Overall Main. Work within this task and report the result here. Turn {turn}. You own this provider's work. {ALTERNATE_STARTUP_PROMPT}{locator} Task: {task}"
                )
            );
            assert!(!prompt.contains(STARTUP_PROMPT.trim()));
            assert!(prompt.chars().count() < CODEX_VISIBLE_PASTE_CHARS);

            // Account mismatch refuses before Room/events checks and before any tmux write.
            spy.calls.borrow_mut().clear();
            spy.room_queries.borrow_mut().clear();
            spy.event_lists.set(0);
            let mut other_host = host.clone();
            let codex_b = private_dir(&spy.repo.parent().unwrap().join("codex-b"));
            other_host.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_a), ("acct-b", codex_b)],
            )]));
            assert!(
                spy.load()
                    .execute_with_host("codex", Some("acct-b"), Some(task), &other_host)
                    .is_err()
            );
            assert!(spy.room_queries.borrow().is_empty());
            assert_eq!(spy.event_lists.get(), 0);
            assert!(spy.mutating_calls().is_empty());

            // Room guard fires in fence(true) with 0 mutating tmux calls.
            spy.calls.borrow_mut().clear();
            spy.room_successor.set(Some("room-successor"));
            let room_err = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some(task), &host)
                .unwrap_err();
            assert!(room_err.message.contains("Main membership"));
            assert!(spy.mutating_calls().is_empty());
            spy.room_successor.set(None);

            // Non-live Main attempt guard fires in fence(true) with 0 mutating tmux calls.
            spy.calls.borrow_mut().clear();
            append_lifecycle_row(&spy.repo, &main_failed_row(4, 1));
            let main_err = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some(task), &host)
                .unwrap_err();
            assert!(main_err.message.contains("live Main attempt"));
            assert!(spy.mutating_calls().is_empty());
        }

        #[test]
        fn codex_primary_task_reference_preserves_payload_and_fences_enter() {
            let (_tmp, spy, host, _) = primary_codex_fixture("primary-ticket");
            let store = Store::open(&spy.repo).unwrap();
            let transaction = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            let send = |task: &str| {
                spy.load().execute_with_host_cached(
                    "codex",
                    Some("acct-a"),
                    Some(task),
                    LeadLaunch::default(),
                    LeadExecution {
                        host: &host,
                        attempts: &AttemptCache::default(),
                        transaction: Some(&transaction),
                    },
                )
            };
            let path_from_prompt = |prompt: &str| {
                let (_, remainder) = prompt.split_once(" Read '").unwrap();
                PathBuf::from(remainder.split_once("' for the assignment;").unwrap().0)
            };
            let reset = || {
                spy.calls.borrow_mut().clear();
                spy.cursor_x.set(2);
                spy.screens
                    .borrow_mut()
                    .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            };
            spy.primary_columns.set(Some(316));
            let brief = concat!(
                "Review the exact fixture;  preserve bytes and explain the résultat for /",
                "home/example/fixture."
            );
            assert_eq!(send(brief).unwrap()["status"], "submitted");
            let mut buffers = spy.calls_named("set-buffer");
            assert_eq!(buffers.len(), 1);
            let prompt = &buffers.pop().unwrap()[5];
            assert!(prompt.is_ascii() && !prompt.contains('\n'));
            assert!(crate::render::width(prompt) + 2 < 316);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert_eq!(spy.calls_named("send-keys").len(), 1);
            let turn = prompt
                .split_once(" Turn ")
                .unwrap()
                .1
                .split_once(". ")
                .unwrap()
                .0;
            let path = path_from_prompt(prompt);
            assert_eq!(
                path,
                spy.repo
                    .join(format!(".oms/artifacts/native-tasks/{turn}.json"))
            );
            let bytes = std::fs::read(&path).unwrap();
            let packet: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(packet["turn"], turn);
            assert_eq!(packet["room"], ROOM);
            assert_eq!(packet["main_attempt"], MAIN);
            assert_eq!(packet["main_participant"], MAIN);
            assert_eq!(packet["lead_pane"]["id"], "%4");
            assert_eq!(packet["account"]["account_id"], "acct-a");
            let locator = assignment_locator(&spy.repo, ROOM).unwrap();
            let full = format!(
                "{} Turn {turn}. You own this provider's work. {ALTERNATE_STARTUP_PROMPT}{locator} Task: {brief}",
                crate::native_children::TASK_PREFIX,
            );
            assert_eq!(packet["payload"], full);
            assert_eq!(
                packet["payload_sha256"],
                oms_panel_core::digest(full.as_bytes())
            );

            for (hook, mutation) in [
                ("set-buffer", "missing"),
                ("set-buffer", "invalid"),
                ("paste-buffer", "foreign"),
                ("paste-buffer", "changed"),
            ] {
                reset();
                spy.task_transition.replace(Some((
                    hook,
                    Box::new(move |spy| {
                        let drafts = spy.recent_screens.borrow();
                        let path = path_from_prompt(&drafts["_pending_paste"]);
                        if mutation == "missing" {
                            std::fs::remove_file(&path).unwrap();
                        } else if mutation == "invalid" {
                            std::fs::write(&path, b"{").unwrap();
                        } else {
                            let mut packet: Value =
                                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                            let field = if mutation == "foreign" {
                                "room"
                            } else {
                                "payload"
                            };
                            packet[field] = mutation.into();
                            std::fs::write(&path, serde_json::to_vec(&packet).unwrap()).unwrap();
                        }
                    }),
                )));
                let error = send(brief).unwrap_err();
                assert!(
                    error
                        .message
                        .contains("task reference is missing or changed")
                );
                assert_eq!(error.publication_uncertain, hook == "paste-buffer");
                assert_eq!(
                    spy.calls_named("paste-buffer").len(),
                    usize::from(hook == "paste-buffer")
                );
                assert!(spy.calls_named("send-keys").is_empty());
                if hook == "paste-buffer" {
                    spy.calls.borrow_mut().clear();
                    assert!(send(brief).is_err());
                    assert!(spy.mutating_calls().is_empty());
                }
            }

            reset();
            spy.task_transition
                .replace(Some(("set-buffer", Box::new(|spy| spy.cursor_x.set(12)))));
            let error = send(brief).unwrap_err();
            assert!(
                error
                    .message
                    .contains("provider cursor is outside the start")
            );
            assert!(!error.publication_uncertain);
            assert!(spy.calls_named("paste-buffer").is_empty());
            assert!(spy.calls_named("send-keys").is_empty());

            let count_records = || std::fs::read_dir(path.parent().unwrap()).unwrap().count();
            let records = count_records();
            reset();
            let sensitive = format!("Review {}{}", "ghp_", "p".repeat(36));
            let error = send(&sensitive).unwrap_err();
            assert!(error.message.contains("task reference content refused"));
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(count_records(), records);

            reset();
            spy.primary_columns.set(Some(0));
            let error = send(brief).unwrap_err();
            assert!(
                error
                    .message
                    .contains("task reference layout is unavailable")
            );
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(count_records(), records);

            for columns in [1200, 145] {
                reset();
                spy.primary_columns.set(Some(columns));
                assert_eq!(send(brief).unwrap()["status"], "submitted");
                assert!(spy.calls_named("set-buffer")[0][5].ends_with(brief));
                assert_eq!(spy.calls_named("send-keys").len(), 1);
                assert_eq!(count_records(), records);
            }

            reset();
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(|spy| {
                    let mut screens = spy.screens.borrow_mut();
                    let draft = screens.get_mut("%4").unwrap();
                    assert!(draft.contains(" \n  "));
                    *draft = draft.replacen(" \n  ", "\n  ", 1);
                }),
            )));
            let error = send(brief).unwrap_err();
            assert!(error.publication_uncertain);
            assert!(spy.calls_named("set-buffer")[0][5].ends_with(brief));
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            assert_eq!(count_records(), records);
        }

        #[test]
        fn codex_primary_over_cap_task_refuses_with_zero_tmux_mutations() {
            let (_tmp, spy, host, _codex_a) = primary_codex_fixture("orch-primary-codex-cap");
            let locator = assignment_locator(&spy.repo, ROOM).unwrap();
            let base_len = format!(
                "Task from Overall Main. Work within this task and report the result here. Turn {}. You own this provider's work. {ALTERNATE_STARTUP_PROMPT}{locator} Task: ",
                "0".repeat(20)
            )
            .chars()
            .count();
            let max_task = "x".repeat(CODEX_VISIBLE_PASTE_CHARS - base_len);
            let ok = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some(&max_task), &host)
                .unwrap();
            assert_eq!(ok["status"], "submitted");
            assert_eq!(
                spy.calls_named("set-buffer")[0][5].chars().count(),
                CODEX_VISIBLE_PASTE_CHARS
            );
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert_eq!(spy.calls_named("send-keys").len(), 1);

            spy.calls.borrow_mut().clear();
            spy.cursor_x.set(2);
            spy.screens
                .borrow_mut()
                .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            let over_task = "x".repeat(CODEX_VISIBLE_PASTE_CHARS - base_len + 1);
            let err = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some(&over_task), &host)
                .unwrap_err();
            assert_eq!(
                err.message,
                "Codex assignment is too long to verify in the composer"
            );
            assert!(!err.publication_uncertain);
            assert!(spy.mutating_calls().is_empty());
            assert!(spy.calls_named("capture-pane").is_empty());
            assert!(spy.calls_named("set-buffer").is_empty());
            assert!(spy.calls_named("paste-buffer").is_empty());
            assert!(spy.calls_named("send-keys").is_empty());

            // Direct unbound Proof::Turn send_checked also refuses > CODEX_VISIBLE_PASTE_CHARS before input probe.
            let over_prompt = "x".repeat(CODEX_VISIBLE_PASTE_CHARS + 1);
            let direct_err = spy
                .load()
                .send("codex", &over_prompt, Proof::Turn(TURN_ID), None)
                .unwrap_err();
            assert_eq!(
                direct_err.message,
                "Codex assignment is too long to verify in the composer"
            );
            assert!(spy.mutating_calls().is_empty());
            assert!(spy.calls_named("capture-pane").is_empty());
        }

        #[test]
        fn codex_collapsed_paste_placeholder_rejects_input_and_withholds_enter() {
            let collapsed_composer = "\x1b[1m›\x1b[0m [Pasted Content 1560 chars]\n";
            let collapsed_screen = format!("{collapsed_composer}{CODEX_STATUS}");
            assert_eq!(
                super::super::codex_input_state(&collapsed_screen),
                Some(false)
            );
            assert!(!input_ready("codex", &collapsed_screen));

            // Existing collapsed placeholder in primary composer rejects submission with 0 mutations.
            let (_tmp, spy, host, _codex_a) = primary_codex_fixture("orch-collapsed-paste");
            spy.cursor_x.set(2);
            spy.screens
                .borrow_mut()
                .insert("%4".into(), collapsed_screen.clone());
            let pre_err = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some("short task"), &host)
                .unwrap_err();
            assert!(pre_err.message.contains("busy or a draft is present"));
            assert!(spy.mutating_calls().is_empty());

            spy.cursor_x.set(31);
            assert!(
                spy.load()
                    .execute_with_host("codex", Some("acct-a"), Some("short task"), &host)
                    .is_err()
            );
            assert!(spy.mutating_calls().is_empty());

            // Ordinary Proof::Turn post-paste collapsed placeholder withholds Enter.
            spy.calls.borrow_mut().clear();
            spy.cursor_x.set(2);
            spy.screens
                .borrow_mut()
                .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            let primary_collapsed = format!("{collapsed_composer}{CODEX_STATUS}");
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(move |spy| {
                    spy.cursor_x.set(31);
                    spy.screens
                        .borrow_mut()
                        .insert("%4".into(), primary_collapsed);
                }),
            )));
            let primary_err = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), Some("short task"), &host)
                .unwrap_err();
            assert!(primary_err.publication_uncertain);
            assert!(
                primary_err
                    .message
                    .contains("Enter was withheld after pasting")
            );
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());

            // Alternate Codex composer rejects collapsed placeholder and paste_and_submit withholds Enter.
            let composer = super::super::CodexComposer {
                command: "codex".into(),
                cursor_x: 31,
                cursor_y: 0,
                screen: collapsed_screen.clone(),
            };
            assert!(!composer.ready());
            assert!(!composer.exact_draft("short task"));

            let (_alt_tmp, alt_spy, alt_host, caller, _home) = alternate_fixture();
            write_synthetic_ledger(&alt_spy.repo, 0);
            alt_spy.real_events.set(true);
            let store = Store::open(&alt_spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(3))
                .unwrap();
            alt_spy
                .load()
                .launch_codex_alternate(&caller, "second", LeadLaunch::default(), &alt_host, &tx)
                .unwrap();
            reset_alternate_assignment(&alt_spy);
            alt_spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(move |spy| {
                    spy.screens
                        .borrow_mut()
                        .insert("%5".into(), collapsed_screen);
                    spy.alternate_task_cursor.set((31, 0));
                }),
            )));
            let err = alt_spy
                .load()
                .assign_codex_alternate(&caller, "second", "short task", &alt_host, &tx)
                .unwrap_err();
            assert!(err.publication_uncertain);
            assert!(err.message.contains("Enter was withheld after pasting"));
            assert_eq!(alt_spy.calls_named("paste-buffer").len(), 1);
            assert!(alt_spy.calls_named("send-keys").is_empty());
        }

        #[test]
        fn non_codex_primary_task_keeps_full_startup_prompt() {
            let tmp = TempDir::new("orch-non-codex-task");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let claude_home = private_dir(&tmp.0.join("claude-home"));
            let uid = rustix::process::getuid().as_raw();
            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![("claude", vec![("work", claude_home)])]));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));
            host.owners.insert(2002, owner_entry(2002, "p2002", uid));
            let spy = SpyPanel::new(repo);
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "fixture-main", uid));
            spy.allow_board_queries.set(true);
            spy.event_projection.replace(Some(json!([{
                "attempt_id": spy.main_attempt,
                "tool": "panel-main",
                "provider": "codex",
                "terminal": false,
                "state": "working",
                "refs": {
                    "panel_role": "main",
                    "panel_room_id": spy.room,
                    "panel_native_scheduling": "unmanaged"
                }
            }])));
            let store = Store::open(&spy.repo).unwrap();
            let tx = store
                .transaction("panel-provider-sessions", Duration::from_secs(1))
                .unwrap();
            let attempts = AttemptCache::default();
            spy.load()
                .execute_with_host("claude", Some("work"), None, &host)
                .unwrap();
            spy.leads.borrow_mut().push(FakeLead {
                window: "@5".into(),
                pane_id: "%5".into(),
                pid: 2002,
                active: false,
                provider: "antigravity".into(),
            });

            let claude_empty = "❯\u{a0}\x1b[2mTry something\x1b[0m\n────\n";
            let agy_empty = "\x1b[38;5;111m>\x1b[39m \x1b[38;5;109mAuto mode: edits & safe commands auto-approved (shift+tab to cycle)\x1b[39m\n────\n";

            for (provider, pane, screen, account, task_text) in [
                (
                    "claude",
                    "%4",
                    claude_empty,
                    Some("work"),
                    "delegate claude work",
                ),
                ("antigravity", "%5", agy_empty, None, "delegate agy work"),
            ] {
                spy.calls.borrow_mut().clear();
                spy.cursor_x.set(2);
                spy.screens.borrow_mut().insert(pane.into(), screen.into());
                spy.task_transition.replace(Some((
                    "paste-buffer",
                    Box::new(move |spy| {
                        let text = spy.recent_screens.borrow()["_pending_paste"].clone();
                        let marker = if provider == "claude" {
                            "❯\u{a0}"
                        } else {
                            "> "
                        };
                        let draft = format!("{marker}{text}");
                        spy.cursor_x.set(crate::render::width(&draft) as u32);
                        spy.screens
                            .borrow_mut()
                            .insert(pane.into(), super::lead_box(&draft));
                        spy.task_transition.replace(Some((
                            "send-keys",
                            Box::new(move |spy| {
                                spy.screens.borrow_mut().insert(pane.into(), screen.into());
                            }),
                        )));
                    }),
                )));
                let out = spy
                    .load()
                    .execute_with_host_cached(
                        provider,
                        account,
                        Some(task_text),
                        LeadLaunch::default(),
                        LeadExecution {
                            host: &host,
                            attempts: &attempts,
                            transaction: Some(&tx),
                        },
                    )
                    .unwrap();
                assert_eq!(out["status"], "unknown");
                assert_eq!(out["delivery"], "input_cleared_unverified");
                assert_eq!(spy.calls_named("send-keys").len(), 1);
                let buffers = spy.calls_named("set-buffer");
                assert_eq!(buffers.len(), 1);
                let prompt = &buffers[0][5];
                assert!(prompt.contains(STARTUP_PROMPT.trim()));
                assert!(prompt.chars().count() > CODEX_VISIBLE_PASTE_CHARS);
                assert!(!prompt.contains(ALTERNATE_STARTUP_PROMPT));
                assert!(!prompt.contains("Read-only Room locator"));
                assert!(prompt.contains(". You own this provider's work. "));
                assert!(prompt.ends_with(&format!("Task: {task_text}")));
            }
        }

        #[test]
        fn canonical_native_assignment_preserves_claim_and_never_replays_unknown_input() {
            const CASE: &str = "OMS_TEST_NATIVE_ASSIGNMENT";
            const TEST: &str = "orchestrator::tests::account_tests::canonical_native_assignment_preserves_claim_and_never_replays_unknown_input";
            let tmp = TempDir::new("orch-task-binding");
            if std::env::var_os(CASE).is_none() {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env_clear()
                    .env("HOME", &tmp.0)
                    .env("PATH", std::env::var_os("PATH").unwrap())
                    .env(CASE, "1")
                    .env("TMUX_PANE", "%1")
                    .env("OMS_PANEL_MAIN_ATTEMPT", MAIN)
                    .env("OMS_ROOM_PARTICIPANT", MAIN)
                    .env("OMS_ROOM_ID", ROOM)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            // Each transport failure is an independent live reservation. Reusing
            // one Lead across these unrelated tasks would violate its contract.
            for scenario in 0..12 {
                let tmp = TempDir::new("orch-native-case");
                let repo = private_dir(&tmp.0.join("repo"));
                let git = |args: &[&str]| {
                    let out = std::process::Command::new("git")
                        .args(args)
                        .current_dir(&repo)
                        .output()
                        .unwrap();
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                };
                git(&["init", "-q"]);
                std::fs::write(repo.join(".gitignore"), ".oms/\n").unwrap();
                git(&["add", ".gitignore"]);
                git(&[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                ]);
                write_synthetic_ledger(&repo, 0);
                let spy = SpyPanel::new(repo.clone());
                spy.allow_board_queries.set(true);
                spy.real_events.set(true);
                spy.canonical_assignment.set(true);
                for args in [
                    commands::strings(&[
                        "new",
                        "--id",
                        ROOM,
                        "--participant",
                        MAIN,
                        "--topic",
                        "Task binding fixture",
                    ]),
                    commands::strings(&[
                        "join",
                        "--id",
                        ROOM,
                        "--participant",
                        MAIN,
                        "--provider",
                        "codex",
                    ]),
                ] {
                    let output = oms_command_room::run(&args, &repo, &spy.stop);
                    assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
                }
                let plan = |args: &[&str]| {
                    let output = oms_command_plan::run(&commands::strings(args), &repo, &spy.stop);
                    assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
                    output.1
                };
                plan(&["init", "--goal", "Bind native work"]);
                for task in [
                    "native-one",
                    "native-two",
                    "native-three",
                    "native-four",
                    "native-five",
                    "native-six",
                ] {
                    plan(&[
                        "add",
                        "--id",
                        task,
                        "--title",
                        "Fixture",
                        "--allowed",
                        "src",
                        "--verify",
                        "true",
                    ]);
                }
                let state = private_dir(&tmp.0.join("state"));
                let codex_home = private_dir(&tmp.0.join("codex"));
                let mut host = FakeHost::new(state);
                host.accounts = Some(make_accounts(vec![("codex", vec![("acct-a", codex_home)])]));
                let uid = rustix::process::getuid().as_raw();
                host.owners
                    .insert(spy.main_pid, owner_entry(spy.main_pid, "fixture-main", uid));
                host.owners.insert(2001, owner_entry(2001, "p2001", uid));
                spy.load()
                    .execute_with_host("codex", Some("acct-a"), None, &host)
                    .unwrap();
                spy.screens
                    .borrow_mut()
                    .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
                let options = |id: &str, body: &str| {
                    Options::parse(&commands::strings(&[
                        "--orchestrator",
                        "codex",
                        "--room",
                        ROOM,
                        "--task-id",
                        id,
                        "--task",
                        body,
                    ]))
                    .unwrap()
                };
                let send = |id, body| {
                    run_with_host(&repo, "codex", &options(id, body), &spy, &spy.stop, &host)
                };
                let retire_unsent = |attempt: &str, intent: &str| {
                    let args = commands::strings(&[
                        "--native-reconcile",
                        attempt,
                        "--intent-sha256",
                        intent,
                        "--decision",
                        "retire-unsent",
                        "--owner",
                        "codex",
                        "--room",
                        ROOM,
                    ]);
                    run_with_host(
                        &repo,
                        "codex",
                        &Options::parse(&args).unwrap(),
                        &spy,
                        &spy.stop,
                        &host,
                    )
                };
                let retain_return = |assignment_result: &Value,
                                     task_id: &str,
                                     candidate: bool|
                 -> Result<Value> {
                    let raw = std::fs::read(repo.join(text(assignment_result, "binding"))).unwrap();
                    let assignment =
                        oms_panel_core::native_contract::CheckedAssignment::parse(&raw)
                            .unwrap()
                            .unwrap();
                    let mut source = assignment.report_template();
                    source["summary"] = "Observed findings only".into();
                    let candidate_path = ".oms/artifacts/return-candidate.patch";
                    let patch = b"diff --git a/src/cancel.rs b/src/cancel.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/cancel.rs\n@@ -0,0 +1 @@\n+candidate\n";
                    if candidate {
                        std::fs::create_dir_all(repo.join(".oms/artifacts")).unwrap();
                        std::fs::write(repo.join(candidate_path), patch).unwrap();
                        source["summary"] = "Returned candidate for owner review".into();
                        source["changed_paths"] = json!(["src/cancel.rs"]);
                        source["candidate_patch"] = candidate_path.into();
                        source["evidence"] = json!([{"path":candidate_path,"sha256":oms_panel_core::digest(patch),"bytes":patch.len()}]);
                    }
                    let source_bytes = serde_json::to_vec(&source).unwrap();
                    let source_path = tmp.0.join("transport-return.json");
                    std::fs::write(&source_path, &source_bytes).unwrap();
                    let mut manifest = json!({"schema":1,"attempt_id":assignment.attempt_id(),"binding_sha256":assignment.sha256(),
                        "kind":"findings","summary":source["summary"],"source":{"path":source_path,"sha256":oms_panel_core::digest(&source_bytes)}});
                    if candidate {
                        manifest["kind"] = "candidate".into();
                        manifest["patch"] = json!({"path":repo.join(candidate_path),"sha256":oms_panel_core::digest(patch)});
                    }
                    let path = tmp.0.join("transport-manifest.json");
                    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                    let args = commands::strings(&[
                        "--orchestrator",
                        "codex",
                        "--room",
                        ROOM,
                        "--task-id",
                        task_id,
                        "--native-result",
                        path.to_str().unwrap(),
                    ]);
                    let output = run_with_host(
                        &repo,
                        "codex",
                        &Options::parse(&args).unwrap(),
                        &spy,
                        &spy.stop,
                        &host,
                    )?;
                    serde_json::from_slice(&output.1).map_err(Error::new)
                };
                spy.calls.borrow_mut().clear();
                if scenario == 0 {
                    // Missing criteria fails before any claim or input side effect.
                    plan(&["add", "--id", "incomplete", "--title", "No verifier"]);
                    spy.calls.borrow_mut().clear();
                    assert!(send("incomplete", "inspect source").is_err());
                    let incomplete: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "incomplete"])).unwrap();
                    assert_eq!(incomplete["state"], "ready");
                    assert!(incomplete["lease_id"].as_str().is_none_or(str::is_empty));
                    assert!(spy.mutating_calls().is_empty());
                    let conversation = |explicit| {
                        let mut args = commands::strings(&[
                            "--orchestrator",
                            "codex",
                            "--room",
                            ROOM,
                            "--task",
                            "Discuss the task framing",
                        ]);
                        if explicit {
                            args.push("--unbound".into());
                        }
                        run_with_host(
                            &repo,
                            "codex",
                            &Options::parse(&args).unwrap(),
                            &spy,
                            &spy.stop,
                            &host,
                        )
                    };
                    assert!(
                        conversation(false)
                            .unwrap_err()
                            .message
                            .contains("canonical work is pending")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    let unbound = conversation(true).unwrap();
                    let unbound: Value = serde_json::from_slice(&unbound.1).unwrap();
                    assert_eq!(unbound["task_binding"], "unbound");
                    assert_eq!(unbound["task_completion"], "unconfirmed");
                    assert_eq!(unbound["acceptance"], "unsupported");
                    // Typed binding covers only ordinary Codex Leads; other providers
                    // keep their untyped --task contract despite pending Plan work.
                    spy.calls.borrow_mut().clear();
                    let claude = run_with_host(
                        &repo,
                        "claude",
                        &Options::parse(&commands::strings(&[
                            "--orchestrator",
                            "claude",
                            "--room",
                            ROOM,
                            "--task",
                            "Discuss the task framing",
                        ]))
                        .unwrap(),
                        &spy,
                        &spy.stop,
                        &host,
                    );
                    // No Claude Lead is open here, so the base path itself answers.
                    let claude = claude.unwrap_err().message;
                    assert!(claude.contains("open the provider session"), "{claude}");
                    assert!(spy.mutating_calls().is_empty());
                    let ready: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-one"])).unwrap();
                    assert_eq!(ready["state"], "ready");
                    spy.calls.borrow_mut().clear();
                    let first = send("native-one", "inspect assigned source").unwrap();
                    assert_eq!(
                        first.0,
                        0,
                        "{} {}",
                        String::from_utf8_lossy(&first.1),
                        String::from_utf8_lossy(&first.2)
                    );
                    let first: Value = serde_json::from_slice(&first.1).unwrap();
                    assert_eq!(first["status"], "submitted");
                    assert_eq!(first["consumption"], "unconfirmed");
                    assert!(
                        spy.calls_named("set-buffer")[0][5].chars().count()
                            <= CODEX_VISIBLE_PASTE_CHARS
                    );
                    // Only registered public verbs may appear in the copyable reader line.
                    let pasted = spy.calls_named("set-buffer")[0][5].clone();
                    assert!(pasted.contains(
                        "oms agent-plan --repo \"$R\" evidence-snapshot --id 'native-one';"
                    ));
                    assert!(!pasted.contains("oms plan "));
                    assert_eq!(spy.calls_named("send-keys").len(), 1);
                    let binding: Value = serde_json::from_slice(
                        &std::fs::read(repo.join(first["binding"].as_str().unwrap())).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(binding["attempt_id"], first["attempt_id"]);
                    assert_eq!(binding["account_verification"], "verified");
                    assert_eq!(binding["task"]["claimed_by_participant"], MAIN);
                    assert_eq!(binding["task"]["lease_epoch"], 1);
                    assert_eq!(binding["checkout"]["worktree"], repo.to_str().unwrap());
                    assert_eq!(binding["checkout"]["write_isolation"], "unmanaged");
                    spy.calls.borrow_mut().clear();
                    let repeated = send("native-one", "inspect assigned source").unwrap();
                    let repeated: Value = serde_json::from_slice(&repeated.1).unwrap();
                    assert_eq!(repeated["attempt_id"], first["attempt_id"]);
                    assert_eq!(repeated["reused"], true);
                    assert!(spy.mutating_calls().is_empty());
                    assert!(
                        send("native-one", "changed scope body")
                            .unwrap_err()
                            .message
                            .contains("brief changed")
                    );
                    assert!(spy.mutating_calls().is_empty());

                    for (field, bad) in [
                        ("title", json!("Changed deliverable")),
                        ("verify", json!("false")),
                        ("allowed_paths", json!(["other"])),
                    ] {
                        let mut overlay = json!({"id":"native-one"});
                        overlay[field] = bad;
                        spy.task_snapshot_override.replace(Some(overlay));
                        assert!(
                            send("native-one", "inspect assigned source")
                                .unwrap_err()
                                .message
                                .contains("frozen criteria"),
                            "accepted {field}"
                        );
                    }
                    spy.task_snapshot_override.replace(None);
                    assert!(spy.mutating_calls().is_empty());
                    // Main ingests the complete exact-bound inventory as untrusted
                    // claims. Replays do not create new reports or provider input.
                    let attempt = text(&first, "attempt_id");
                    let binding_bytes = std::fs::read(repo.join(text(&first, "binding"))).unwrap();
                    let assignment =
                        oms_panel_core::native_contract::CheckedAssignment::parse(&binding_bytes)
                            .unwrap()
                            .unwrap();
                    assert_eq!(assignment.requirement_ids(), &["native-one"]);
                    let report_file = private_dir(&repo.join(".oms/artifacts")).join("return.json");
                    let input_path = tmp.0.join("native-manifest.json");
                    let predecessor = RefCell::new(None::<String>);
                    let write_report = |report: &Value| {
                        let raw = serde_json::to_vec(report).unwrap();
                        std::fs::write(&report_file, &raw).unwrap();
                        let patch = report["candidate_patch"].as_str();
                        let mut manifest = json!({"schema":1,"attempt_id":attempt,
                            "binding_sha256":report["assignment_sha256"],
                            "kind":if patch.is_some() {"candidate"} else {"findings"},
                            "summary":report["summary"],"source":{"path":report_file,"sha256":oms_panel_core::digest(&raw)},
                            "previous_sha256":predecessor.borrow().as_deref().unwrap_or("")});
                        let reference =
                            |row: &Value| json!({"path":row["path"],"sha256":row["sha256"]});
                        if let Some(patch) = patch {
                            manifest["patch"] = reference(
                                crate::rows(&report["evidence"])
                                    .iter()
                                    .find(|e| e["path"] == patch)
                                    .unwrap(),
                            );
                        }
                        manifest["verification"] = crate::rows(&report["evidence"])
                            .iter()
                            .filter(|e| Some(text(e, "path")) != patch)
                            .map(reference)
                            .collect::<Vec<_>>()
                            .into();
                        std::fs::write(&input_path, serde_json::to_vec(&manifest).unwrap())
                            .unwrap();
                    };
                    let invoke = |args: &[&str]| {
                        let mut args = commands::strings(args);
                        args.extend(commands::strings(&["--room", ROOM]));
                        run_with_host(
                            &repo,
                            "codex",
                            &Options::parse(&args).unwrap(),
                            &spy,
                            &spy.stop,
                            &host,
                        )
                    };
                    let ingest_args = [
                        "--orchestrator",
                        "codex",
                        "--task-id",
                        "native-one",
                        "--native-result",
                        input_path.to_str().unwrap(),
                    ];
                    let mut report = assignment.report_template();
                    report["summary"] = "Implementation requires a narrower follow-up".into();
                    report["checks"] = json!([{"name":"declared check","status":"passed","evidence":[],"note":"Lead claim only"}]);
                    let mut invalid = report.clone();
                    invalid["lease_epoch"] = 2.into();
                    write_report(&invalid);
                    assert!(
                        invoke(&ingest_args)
                            .unwrap_err()
                            .message
                            .contains("exact assignment")
                    );
                    write_report(&report);
                    let events_path = repo.join(".oms/lifecycle/events.jsonl");
                    let before_report = std::fs::read(&events_path).unwrap();
                    let captured =
                        oms_panel_core::native_results::CaptureInput::read(&repo, &input_path)
                            .unwrap()
                            .capture(&repo, Some(&assignment))
                            .unwrap();
                    let frozen = {
                        let store = Store::open(&repo).unwrap();
                        let tx = store
                            .transaction("native-result-fixture", Duration::from_secs(3))
                            .unwrap();
                        captured
                            .retain(&tx, assignment.packet(), text(&first, "binding"))
                            .unwrap()
                    };
                    assert_eq!(std::fs::read(&events_path).unwrap(), before_report);
                    let returned = invoke(&ingest_args).unwrap();
                    let returned: Value = serde_json::from_slice(&returned.1).unwrap();
                    assert_eq!(returned["result"], frozen);
                    assert_eq!(returned["task_completion"], "unconfirmed");
                    assert_eq!(returned["acceptance_eligible"], false);
                    let first_report_sha = text(&returned["result"], "sha256").to_owned();
                    let repeated = invoke(&ingest_args).unwrap();
                    let repeated: Value = serde_json::from_slice(&repeated.1).unwrap();
                    assert_eq!(repeated["reused"], true);
                    assert!(
                        invoke(
                            &[
                                ingest_args.as_slice(),
                                &["--expected-previous-result-sha256", &"0".repeat(64)]
                            ]
                            .concat()
                        )
                        .is_err()
                    );
                    assert!(spy.mutating_calls().is_empty());
                    // Report correction is explicit CAS, separate from a new Lead round.
                    report["summary"] = "Corrected report with explicit revision".into();
                    report["revision"] = 2.into();
                    report["previous_report_sha256"] =
                        oms_panel_core::digest(&std::fs::read(&report_file).unwrap()).into();
                    predecessor.replace(Some(first_report_sha.clone()));
                    write_report(&report);
                    assert!(invoke(&ingest_args).is_err());
                    let corrected = invoke(
                        &[
                            ingest_args.as_slice(),
                            &["--expected-previous-result-sha256", &first_report_sha],
                        ]
                        .concat(),
                    )
                    .unwrap();
                    let corrected: Value = serde_json::from_slice(&corrected.1).unwrap();
                    let report_sha = text(&corrected["result"], "sha256").to_owned();
                    let decision = |sha: &str, kind: &str| {
                        invoke(&[
                            "--native-reconcile",
                            attempt,
                            "--result-sha256",
                            sha,
                            "--decision",
                            kind,
                        ])
                    };
                    assert!(decision(&first_report_sha, "request-revision").is_err());
                    assert!(
                        send("native-two", "unresolved assignment cannot be replaced")
                            .unwrap_err()
                            .message
                            .contains("unresolved task")
                    );
                    decision(&report_sha, "blocked").unwrap();
                    let next_round = |sha: &str, brief: &str| {
                        let args = commands::strings(&[
                            "--orchestrator",
                            "codex",
                            "--room",
                            ROOM,
                            "--task-id",
                            "native-one",
                            "--task",
                            brief,
                            "--after-native-result",
                            sha,
                        ]);
                        run_with_host(
                            &repo,
                            "codex",
                            &Options::parse(&args).unwrap(),
                            &spy,
                            &spy.stop,
                            &host,
                        )
                    };
                    assert!(next_round(&report_sha, "Address the review finding").is_err());
                    decision(&report_sha, "request-revision").unwrap();
                    let round = next_round(&report_sha, "Address the review finding").unwrap();
                    assert_eq!(round.0, 0);
                    let round: Value = serde_json::from_slice(&round.1).unwrap();
                    assert_eq!(round["attempt_id"], attempt);
                    assert_eq!(spy.calls_named("send-keys").len(), 1);
                    spy.calls.borrow_mut().clear();
                    let repeated = next_round(&report_sha, "Address the review finding").unwrap();
                    let repeated: Value = serde_json::from_slice(&repeated.1).unwrap();
                    assert_eq!(repeated["reused"], true);
                    assert!(spy.mutating_calls().is_empty());
                    assert!(next_round(&report_sha, "Silently changed continuation").is_err());
                    assert_eq!(
                        std::fs::read(repo.join(text(&first, "binding"))).unwrap(),
                        binding_bytes
                    );
                    let new_bytes = std::fs::read(repo.join(text(&round, "binding"))).unwrap();
                    let current_assignment =
                        oms_panel_core::native_contract::CheckedAssignment::parse(&new_bytes)
                            .unwrap()
                            .unwrap();
                    assert_eq!(current_assignment.round(), 2);
                    assert_eq!(current_assignment.lease_id(), assignment.lease_id());
                    assert_eq!(
                        current_assignment.packet()["previous_result_sha256"],
                        report_sha
                    );
                    assert!(repo.join(text(&corrected["result"], "path")).is_file());
                    predecessor.replace(None);
                    // A candidate uses captured patch bytes and exact declared paths;
                    // it reaches Plan Review, never acceptance or native cleanup.
                    let patch = b"diff --git a/src/value.txt b/src/value.txt\nnew file mode 100644\n--- /dev/null\n+++ b/src/value.txt\n@@ -0,0 +1 @@\n+answer\n";
                    let patch_name = ".oms/artifacts/candidate.patch";
                    std::fs::write(repo.join(patch_name), patch).unwrap();
                    let mut candidate = current_assignment.report_template();
                    candidate["summary"] = "Candidate for parent verification".into();
                    candidate["candidate_patch"] = patch_name.into();
                    candidate["changed_paths"] = json!(["src/wrong.txt"]);
                    let verifier_name = ".oms/artifacts/native-verifier.txt";
                    let verifier = b"Reported checks passed, exit 0.\n";
                    std::fs::write(repo.join(verifier_name), verifier).unwrap();
                    candidate["evidence"] = json!([
                        {"path":patch_name,"sha256":oms_panel_core::digest(patch),"bytes":patch.len()},
                        {"path":verifier_name,"sha256":oms_panel_core::digest(verifier),"bytes":verifier.len()}]);
                    candidate["checks"] = json!([{"name":"reported check","status":"passed", "evidence":[verifier_name],"note":"untrusted Lead claim"}]);
                    write_report(&candidate);
                    assert!(invoke(&ingest_args).unwrap_err().message.contains("paths"));
                    candidate["changed_paths"] = json!(["src/value.txt"]);
                    write_report(&candidate);
                    let returned = invoke(&ingest_args).unwrap();
                    let returned: Value = serde_json::from_slice(&returned.1).unwrap();
                    let candidate_sha = text(&returned["result"], "sha256").to_owned();
                    // Preserve the original importer regression matrix while
                    // exercising the new typed source through that same writer.
                    let store = Store::open(&repo).unwrap();
                    let view = || {
                        oms_panel_core::results::results(&repo, Some("native-one"), &spy, &spy.stop)
                            .unwrap()
                    };
                    let task_view = view();
                    let row = &task_view["rows"][0];
                    let captured_report = &row["native_assignment"]["report"];
                    assert_eq!(captured_report["status"], "reported");
                    assert_eq!(captured_report["producer"], "owner-import");
                    assert_eq!(captured_report["verification_state"], "reported");
                    assert_eq!(row["acceptance"], false);
                    assert_eq!(row["admitted"], false);
                    assert_eq!(row["stages"]["verify"]["state"], "not_run");
                    assert_eq!(row["stages"]["review"]["state"], "not_run");
                    assert!(crate::rows(&row["calls"]).is_empty());
                    let operation =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    for field in [
                        "parent_attempt_id",
                        "task_id",
                        "plan_id",
                        "lease_id",
                        "panel_room_id",
                        "native_binding_sha256",
                    ] {
                        let mut foreign = operation.clone();
                        if ["parent_attempt_id", "task_id"].contains(&field) {
                            foreign[field] = "foreign".into();
                        } else {
                            foreign["refs"][field] = "foreign".into();
                        }
                        assert!(
                            oms_panel_core::native_results::read(&store, &foreign).is_err(),
                            "accepted {field}"
                        );
                    }
                    let refused = oms_panel_core::results::accept_native(
                        &repo,
                        &oms_panel_core::results::AcceptRequest {
                            owner: "codex".into(),
                            task_id: "native-one".into(),
                            main_attempt_id: MAIN.into(),
                            message: "Inspect candidate".into(),
                            verify: "true".into(),
                            execution_repo: repo.clone(),
                            expected_patch_sha256: Some(oms_panel_core::digest(patch)),
                            allow_verifier_change: false,
                            allow_floor_change: None,
                        },
                        &spy,
                        &spy.stop,
                    )
                    .unwrap_err();
                    assert!(
                        refused.message.contains("live or unconfirmed"),
                        "{}",
                        refused.message
                    );
                    for (mode, expected) in [
                        (
                            crate::reader::View::Raw,
                            "Candidate for parent verification",
                        ),
                        (crate::reader::View::Diff, "+answer"),
                        (crate::reader::View::Files, "src/value.txt +1 -0"),
                        (crate::reader::View::Info, "native-codex"),
                    ] {
                        let mut reader = crate::reader::Reader::default();
                        reader.view = mode;
                        assert!(
                            reader
                                .render(&repo, &task_view, 120)
                                .unwrap()
                                .join("")
                                .contains(expected),
                            "missing {expected}"
                        );
                    }
                    let manifest: Value =
                        serde_json::from_slice(&std::fs::read(&input_path).unwrap()).unwrap();
                    let import_manifest = |manifest: &Value, host: &FakeHost| {
                        std::fs::write(&input_path, serde_json::to_vec(manifest).unwrap()).unwrap();
                        let mut args = commands::strings(&ingest_args);
                        args.extend(commands::strings(&["--room", ROOM]));
                        run_with_host(
                            &repo,
                            "codex",
                            &Options::parse(&args).unwrap(),
                            &spy,
                            &spy.stop,
                            host,
                        )
                    };
                    let after_candidate = std::fs::read(&events_path).unwrap();
                    for field in [
                        "binding_sha256",
                        "source",
                        "patch",
                        "verification",
                        "kind",
                        "consumption",
                    ] {
                        let mut invalid = manifest.clone();
                        match field {
                            "source" | "patch" => invalid[field]["sha256"] = "0".repeat(64).into(),
                            "verification" => invalid[field][0]["sha256"] = "0".repeat(64).into(),
                            "kind" => invalid[field] = "findings".into(),
                            "consumption" => invalid[field] = "confirmed".into(),
                            _ => invalid[field] = "0".repeat(64).into(),
                        }
                        assert!(
                            import_manifest(&invalid, &host).is_err(),
                            "accepted {field}"
                        );
                    }
                    for target in ["outside/value.txt", "src"] {
                        let mut outside = manifest.clone();
                        let bytes = std::str::from_utf8(patch)
                            .unwrap()
                            .replace("src/value.txt", target);
                        std::fs::write(repo.join(patch_name), &bytes).unwrap();
                        outside["patch"]["sha256"] =
                            oms_panel_core::digest(bytes.as_bytes()).into();
                        assert!(import_manifest(&outside, &host).is_err());
                    }
                    std::fs::write(repo.join(patch_name), patch).unwrap();
                    let sample = concat!("gh", "p_12345678901234567890abcd");
                    let escaped = sample
                        .chars()
                        .map(|c| format!("\\u{:04x}", c as u32))
                        .collect::<String>();
                    let mut sensitive = manifest.clone();
                    sensitive["summary"] = sample.into();
                    std::fs::write(
                        &input_path,
                        serde_json::to_string(&sensitive)
                            .unwrap()
                            .replace(sample, &escaped),
                    )
                    .unwrap();
                    assert!(
                        oms_panel_core::native_results::CaptureInput::read(&repo, &input_path)
                            .is_err()
                    );
                    for reference in [
                        captured_report["artifact"].as_str().unwrap(),
                        text(&captured_report["input"], "path"),
                        text(&captured_report["source"], "path"),
                        text(&captured_report["patch"], "path"),
                        text(&captured_report["verification"][0], "path"),
                    ] {
                        let path = repo.join(reference);
                        let original = std::fs::read(&path).unwrap();
                        std::fs::write(&path, b"changed evidence").unwrap();
                        assert!(oms_panel_core::native_results::read(&store, &operation).is_err());
                        std::fs::write(path, original).unwrap();
                    }
                    let mut moved_host = host.clone();
                    moved_host.accounts = Some(make_accounts(vec![(
                        "codex",
                        vec![("acct-a", private_dir(&tmp.0.join("other-account")))],
                    )]));
                    assert!(import_manifest(&manifest, &moved_host).is_err());
                    let mut reused_host = host.clone();
                    reused_host
                        .owners
                        .insert(2001, owner_entry(2001, "reused-lead", uid));
                    assert!(import_manifest(&manifest, &reused_host).is_err());
                    spy.session_reply.replace(Some(out(format!(
                        "$1\t{}\tchanged-socket\t.\n{}\n",
                        spy.server_pid,
                        repo.display()
                    ))));
                    assert!(import_manifest(&manifest, &host).is_err());
                    let mut moved_report = candidate.clone();
                    moved_report["summary"] = "Candidate after lead move".into();
                    moved_report["revision"] = 2.into();
                    moved_report["previous_report_sha256"] =
                        oms_panel_core::digest(&std::fs::read(&report_file).unwrap()).into();
                    predecessor.replace(Some(candidate_sha.clone()));
                    write_report(&moved_report);
                    let moved_manifest = std::fs::read(&input_path).unwrap();
                    let import_moved = || {
                        std::fs::write(&input_path, &moved_manifest).unwrap();
                        invoke(
                            &[
                                ingest_args.as_slice(),
                                &["--expected-previous-result-sha256", &candidate_sha],
                            ]
                            .concat(),
                        )
                    };
                    let original_window = spy.leads.borrow()[0].window.clone();
                    spy.task_transition.replace(Some((
                        "list-panes",
                        Box::new(|spy| {
                            spy.leads.borrow_mut()[0].window = "@9".into();
                        }),
                    )));
                    let moved = import_moved().unwrap_err();
                    assert!(
                        moved
                            .message
                            .contains("Main, Codex lead, or session changed"),
                        "{}",
                        moved.message
                    );
                    assert!(spy.task_transition.borrow().is_none());
                    spy.leads.borrow_mut()[0].window = original_window.clone();
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_candidate);
                    let lead_pane = spy.leads.borrow()[0].pane_id.clone();
                    spy.task_transition.replace(Some((
                        "list-panes",
                        Box::new({
                            let lead_pane = lead_pane.clone();
                            move |spy| {
                                spy.task_transition.replace(Some((
                                    "list-panes",
                                    Box::new(move |spy| {
                                        spy.alternate_parents
                                            .borrow_mut()
                                            .insert(lead_pane.clone(), "retagged-parent".into());
                                    }),
                                )));
                            }
                        }),
                    )));
                    let retagged = import_moved().unwrap_err();
                    assert!(
                        retagged
                            .message
                            .contains("Main, Codex lead, or session changed"),
                        "{}",
                        retagged.message
                    );
                    assert!(spy.task_transition.borrow().is_none());
                    spy.alternate_parents.borrow_mut().remove(&lead_pane);
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_candidate);
                    predecessor.replace(None);
                    write_report(&candidate);
                    // The same fresh-owner fence applies to Main's new decision
                    // path before it can write a canonical Plan Review.
                    spy.task_transition.replace(Some((
                        "list-panes",
                        Box::new(|spy| {
                            spy.leads.borrow_mut()[0].window = "@9".into();
                        }),
                    )));
                    assert!(
                        decision(&candidate_sha, "reviewed")
                            .unwrap_err()
                            .message
                            .contains("Main, Codex lead, or session changed")
                    );
                    assert!(spy.task_transition.borrow().is_none());
                    spy.leads.borrow_mut()[0].window = original_window;
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_candidate);
                    let still_claimed: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-one"])).unwrap();
                    assert_eq!(still_claimed["state"], "claimed");
                    git(&[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.invalid",
                        "commit",
                        "--allow-empty",
                        "-qm",
                        "new base",
                    ]);
                    assert!(
                        import_manifest(&manifest, &host)
                            .unwrap_err()
                            .message
                            .contains("base")
                    );
                    git(&["reset", "--soft", current_assignment.base_sha()]);
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_candidate);
                    assert!(spy.mutating_calls().is_empty());
                    import_manifest(&manifest, &host).unwrap();
                    spy.task_transition.replace(Some((
                        "plan-review",
                        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
                    )));
                    assert!(decision(&candidate_sha, "reviewed").is_err());
                    spy.stop.store(false, Ordering::Release);
                    let reviewed: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-one"])).unwrap();
                    assert_eq!(reviewed["state"], "review");
                    assert_eq!(reviewed["review_lease_id"], assignment.lease_id());
                    let retained = oms_panel_core::native_results::read(
                        &Store::open(&repo).unwrap(),
                        &native_assignment::current_operation(&spy.load(), attempt).unwrap(),
                    )
                    .unwrap()
                    .unwrap();
                    let reviewed_patch = text(&retained["patch"], "path").to_owned();
                    assert_eq!(reviewed["patch"], reviewed_patch);
                    assert_eq!(std::fs::read(repo.join(&reviewed_patch)).unwrap(), patch);
                    assert_eq!(
                        reviewed["review_evidence"]["artifact"]["sha256"],
                        candidate_sha
                    );
                    assert_eq!(
                        reviewed["review_evidence"]["patch"]["sha256"],
                        retained["patch"]["sha256"]
                    );
                    spy.task_snapshot_override.replace(Some(json!({
                        "id":"native-one", "review_evidence":null,
                    })));
                    assert!(decision(&candidate_sha, "reviewed").is_err());
                    spy.task_snapshot_override.replace(None);
                    // A crash after canonical Review before the resolution is repairable
                    // with the same frozen report/lease/artifact and no second Review.
                    let reconciled = decision(&candidate_sha, "reviewed").unwrap();
                    let reconciled: Value = serde_json::from_slice(&reconciled.1).unwrap();
                    assert_eq!(reconciled["acceptance"], "pending");
                    let repeated = decision(&candidate_sha, "reviewed").unwrap();
                    let repeated: Value = serde_json::from_slice(&repeated.1).unwrap();
                    assert_eq!(repeated["reused"], true);
                    let operation =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(operation["terminal"], false);
                    assert_eq!(operation["refs"]["native_resolution"], "reviewed");
                    spy.task_snapshot_override.replace(Some(json!({
                        "id":"native-one", "state":"claimed", "review_evidence":null,
                    })));
                    assert!(decision(&candidate_sha, "reviewed").is_err());
                    spy.task_snapshot_override.replace(None);
                    assert!(
                        send("native-two", "review is not completion")
                            .unwrap_err()
                            .message
                            .contains("unresolved task")
                    );
                    let finalize = oms_panel_core::results::FinalizeRequest {
                        owner: "codex".into(),
                        task_id: "native-one".into(),
                        main_attempt_id: MAIN.into(),
                        summary_file: report_file.clone(),
                        outcome: "accepted".into(),
                        verify: Some("true".into()),
                        evidence: vec![],
                        verify_timeout: Duration::from_secs(1),
                        execution_repo: repo.clone(),
                        notify: false,
                        landed_commit: None,
                    };
                    let error = oms_panel_core::results::finalize_snapshot(
                        &repo, &finalize, &spy, &spy.stop,
                    )
                    .unwrap_err();
                    assert!(
                        error.message.contains("live or unconfirmed"),
                        "{}",
                        error.message
                    );
                    assert!(spy.calls_named("kill-pane").is_empty());
                    // The canonical CLI landing suite proves patch-land→done.
                    // Here that completion-query port supplies the exact settled
                    // tuple so real lifecycle/Room writers can be fault-injected
                    // at both retirement boundaries without inventing receipts.
                    for (field, bad) in [
                        ("lease_id", json!(format!("lease_{}", "0".repeat(32)))),
                        ("lease_epoch", json!(2)),
                        ("provider", json!("claude")),
                        ("claimed_by_participant", json!("foreign")),
                    ] {
                        let mut overlay = json!({"id":"native-one","state":"done"});
                        overlay[field] = bad;
                        spy.task_snapshot_override.replace(Some(overlay));
                        assert!(
                            decision(&candidate_sha, "reviewed").is_err(),
                            "accepted foreign {field}"
                        );
                    }
                    spy.task_snapshot_override.replace(Some(
                        json!({"id":"native-one","state":"done","completion_verified":true}),
                    ));
                    let before_retirement = std::fs::read(&events_path).unwrap();
                    spy.task_transition.replace(Some((
                        "list-panes",
                        Box::new(|spy| spy.main_owner.set("claude")),
                    )));
                    assert!(decision(&candidate_sha, "reviewed").is_err());
                    assert!(spy.task_transition.borrow().is_none());
                    spy.main_owner.set("codex");
                    assert_eq!(std::fs::read(&events_path).unwrap(), before_retirement);
                    spy.task_transition.replace(Some((
                        "room-leave",
                        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
                    )));
                    assert!(decision(&candidate_sha, "reviewed").is_err());
                    spy.stop.store(false, Ordering::Release);
                    let retiring =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(retiring["terminal"], true);
                    assert_eq!(retiring["refs"]["native_resolution"], "retiring");
                    let room = commands::query(
                        &spy,
                        NativeCommand::Room,
                        &repo,
                        &["show", "--id", ROOM, "--exact", "--json"],
                        &spy.stop,
                    )
                    .unwrap();
                    assert!(
                        crate::rows(&room["participants"])
                            .iter()
                            .any(|p| p["participant"] == attempt && p["joined"] == true)
                    );
                    spy.task_transition.replace(Some((
                        "room-leave-recorded",
                        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
                    )));
                    let interrupted_retirement = decision(&candidate_sha, "reviewed").unwrap_err();
                    spy.stop.store(false, Ordering::Release);
                    let retiring =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(retiring["refs"]["native_resolution"], "retiring");
                    let room = commands::query(
                        &spy,
                        NativeCommand::Room,
                        &repo,
                        &["show", "--id", ROOM, "--exact", "--json"],
                        &spy.stop,
                    )
                    .unwrap();
                    assert!(
                        crate::rows(&room["participants"])
                            .iter()
                            .any(|p| p["participant"] == attempt && p["joined"] == false),
                        "{}",
                        interrupted_retirement.message
                    );
                    let retired = decision(&candidate_sha, "reviewed").unwrap();
                    let retired: Value = serde_json::from_slice(&retired.1).unwrap();
                    assert_eq!(retired["status"], "retired");
                    assert_eq!(retired["child_cleanup"], "unsupported");
                    let before = std::fs::read(&events_path).unwrap();
                    decision(&candidate_sha, "reviewed").unwrap();
                    assert_eq!(std::fs::read(&events_path).unwrap(), before);
                    assert_eq!(spy.leads.borrow().len(), 1);
                    assert!(spy.calls_named("kill-pane").is_empty());
                    let next = send("native-two", "Next canonical task").unwrap();
                    let next: Value = serde_json::from_slice(&next.1).unwrap();
                    assert_eq!(next["status"], "submitted");
                    assert_ne!(next["attempt_id"], attempt);
                    spy.task_snapshot_override.replace(None);
                    let original: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-one"])).unwrap();
                    assert_eq!(original["state"], "review"); // no synthetic Plan completion was written
                    // A released and re-claimed Plan task has a new lease_id: the
                    // otherwise valid typed manifest is refused before any write.
                    let raw = std::fs::read(repo.join(text(&next, "binding"))).unwrap();
                    let bound = oms_panel_core::native_contract::CheckedAssignment::parse(&raw)
                        .unwrap()
                        .unwrap();
                    let mut lease_source = bound.report_template();
                    lease_source["summary"] = "Observed after lease change".into();
                    let lease_bytes = serde_json::to_vec(&lease_source).unwrap();
                    let lease_source_path = tmp.0.join("lease-return.json");
                    std::fs::write(&lease_source_path, &lease_bytes).unwrap();
                    let lease_manifest_path = tmp.0.join("lease-manifest.json");
                    std::fs::write(
                        &lease_manifest_path,
                        serde_json::to_vec(&json!({"schema":1,"attempt_id":bound.attempt_id(),
                            "binding_sha256":bound.sha256(),"kind":"findings",
                            "summary":lease_source["summary"],
                            "source":{"path":lease_source_path,"sha256":oms_panel_core::digest(&lease_bytes)}}))
                        .unwrap(),
                    )
                    .unwrap();
                    let lease_options = Options::parse(&commands::strings(&[
                        "--orchestrator",
                        "codex",
                        "--room",
                        ROOM,
                        "--task-id",
                        "native-two",
                        "--native-result",
                        lease_manifest_path.to_str().unwrap(),
                    ]))
                    .unwrap();
                    let retained_dir = repo
                        .join(".oms/artifacts/native-results")
                        .join(bound.attempt_id());
                    let listing = || {
                        let mut names: Vec<_> = std::fs::read_dir(&retained_dir)
                            .map(|d| d.map(|e| e.unwrap().path()).collect())
                            .unwrap_or_default();
                        names.sort();
                        names
                    };
                    let plan_show = |id: &str| -> Value {
                        serde_json::from_slice(&plan(&["show", "--id", id])).unwrap()
                    };
                    let old_lease = text(&plan_show("native-two"), "lease_id").to_owned();
                    plan(&["release", "--id", "native-two"]);
                    plan(&["claim", "--id", "native-two", "--provider", "codex"]);
                    assert_ne!(text(&plan_show("native-two"), "lease_id"), old_lease);
                    let lease_events =
                        std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
                    let lease_listing = listing();
                    spy.calls.borrow_mut().clear();
                    let lease_err =
                        run_with_host(&repo, "codex", &lease_options, &spy, &spy.stop, &host)
                            .unwrap_err();
                    assert!(lease_err.message.contains("lease"), "{}", lease_err.message);
                    assert_eq!(
                        std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
                        lease_events
                    );
                    assert!(spy.mutating_calls().is_empty());
                    assert_eq!(listing(), lease_listing);
                }
                if scenario == 1 {
                    // Enter happened, then the caller lost the ability to record a
                    // response. The durable intent survives and a retry sends nothing.
                    spy.task_transition.replace(Some((
                        "send-keys",
                        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
                    )));
                    let unknown = send("native-two", "second assignment").unwrap();
                    assert_eq!(unknown.0, 75);
                    let unknown: Value = serde_json::from_slice(&unknown.1).unwrap();
                    assert_eq!(unknown["status"], "unknown");
                    spy.stop.store(false, Ordering::Release);
                    spy.calls.borrow_mut().clear();
                    let attempt = text(&unknown, "attempt_id");
                    let operation =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    let intent = text(&operation["refs"], "native_intent_sha256");
                    let events_path = repo.join(".oms/lifecycle/events.jsonl");
                    let before = std::fs::read(&events_path).unwrap();
                    let refusal = retire_unsent(attempt, intent).unwrap_err();
                    assert!(
                        refusal.message.contains("original pre-binding"),
                        "{}",
                        refusal.message
                    );
                    assert_eq!(std::fs::read(&events_path).unwrap(), before);
                    let repeated = send("native-two", "second assignment").unwrap();
                    assert_eq!(repeated.0, 75);
                    let repeated: Value = serde_json::from_slice(&repeated.1).unwrap();
                    assert_eq!(repeated["attempt_id"], unknown["attempt_id"]);
                    assert!(spy.mutating_calls().is_empty());
                    let next_action = || {
                        let view = oms_panel_core::results::results(
                            &repo,
                            Some("native-two"),
                            &spy,
                            &spy.stop,
                        )
                        .unwrap();
                        text(&view["rows"][0], "next_action").to_owned()
                    };
                    // Typed projection: no report yet -> reconcile, never resend.
                    assert_eq!(
                        next_action(),
                        "native input submission is unconfirmed; do not resend"
                    );
                    let reported = retain_return(&unknown, "native-two", false).unwrap();
                    assert_eq!(reported["consumption"], "unconfirmed");
                    assert!(matches!(
                        reported["transport"].as_str(),
                        Some("sending" | "unknown")
                    ));
                    // The imported report moves the typed next step to Main review
                    // while the unconfirmed transport still forbids a resend.
                    assert_eq!(
                        next_action(),
                        "Main must review the returned claims and run the frozen verifier"
                    );
                    let before = std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
                    assert_eq!(
                        retain_return(&unknown, "native-two", false).unwrap()["reused"],
                        true
                    );
                    assert_eq!(
                        std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
                        before
                    );
                    assert!(spy.mutating_calls().is_empty());

                    // If Main transitions to terminal after the final pre-append
                    // revalidation sample and before append_lifecycle_checked_with_projection
                    // acquires the ledger lock, register_result, native_assignment::annotate,
                    // and native_result::annotate must refuse without marking
                    // publication_uncertain or appending any event.
                    let events_path = repo.join(".oms/lifecycle/events.jsonl");
                    let store = Store::open(&repo).unwrap();
                    let unknown_before = native_assignment::current_operation(
                        &spy.load(),
                        text(&unknown, "attempt_id"),
                    )
                    .unwrap();
                    let raw = std::fs::read(repo.join(text(&unknown, "binding"))).unwrap();
                    let assignment =
                        oms_panel_core::native_contract::CheckedAssignment::parse(&raw)
                            .unwrap()
                            .unwrap();
                    let prior_source_bytes =
                        std::fs::read(tmp.0.join("transport-return.json")).unwrap();
                    let mut race_source = assignment.report_template();
                    race_source["summary"] = "Report racing terminal Main".into();
                    race_source["revision"] = 2.into();
                    race_source["previous_report_sha256"] =
                        oms_panel_core::digest(&prior_source_bytes).into();
                    let race_source_bytes = serde_json::to_vec(&race_source).unwrap();
                    let race_source_path = tmp.0.join("race-return.json");
                    std::fs::write(&race_source_path, &race_source_bytes).unwrap();
                    let prior_result_sha = text(&reported["result"], "sha256");
                    let race_manifest = json!({
                        "schema": 1,
                        "attempt_id": assignment.attempt_id(),
                        "binding_sha256": assignment.sha256(),
                        "kind": "findings",
                        "summary": race_source["summary"],
                        "previous_sha256": prior_result_sha,
                        "source": {
                            "path": race_source_path,
                            "sha256": oms_panel_core::digest(&race_source_bytes),
                        },
                    });
                    let race_manifest_path = tmp.0.join("race-manifest.json");
                    std::fs::write(
                        &race_manifest_path,
                        serde_json::to_vec(&race_manifest).unwrap(),
                    )
                    .unwrap();
                    let race_options = Options::parse(&commands::strings(&[
                        "--orchestrator",
                        "codex",
                        "--room",
                        ROOM,
                        "--task-id",
                        "native-two",
                        "--native-result",
                        race_manifest_path.to_str().unwrap(),
                        "--expected-previous-result-sha256",
                        prior_result_sha,
                    ]))
                    .unwrap();
                    let race_panel = spy.load();
                    let race_attempts = AttemptCache::default();
                    race_panel
                        .require_main_with_cache(race_options.get("owner"), &race_attempts)
                        .unwrap();
                    let race_tx = store
                        .transaction("native-result-main-terminal-race", Duration::from_secs(3))
                        .unwrap();
                    let before_race_lines = std::fs::read_to_string(&events_path)
                        .unwrap()
                        .lines()
                        .count();
                    let mut main_terminated = false;
                    let race_err = native_assignment::register_result_with_before_append(
                        &race_panel,
                        &race_options,
                        &host,
                        &race_attempts,
                        &race_tx,
                        || {
                            for to_state in ["blocked", "abandoned"] {
                                let event = oms_command_operations::new_event(
                                    MAIN,
                                    0usize.into(),
                                    "attempt.state_changed",
                                    oms_command_operations::value::obj([
                                        ("to_state", to_state.into()),
                                        (
                                            "actor",
                                            oms_command_operations::value::obj([
                                                ("kind", "owner".into()),
                                                ("name", "test-main-terminal".into()),
                                            ]),
                                        ),
                                    ]),
                                )
                                .unwrap();
                                oms_command_operations::append_lifecycle(&repo, event, &spy.stop)
                                    .unwrap();
                            }
                            main_terminated = true;
                        },
                    )
                    .unwrap_err();
                    assert!(main_terminated);
                    assert!(!race_err.publication_uncertain);
                    assert!(
                        race_err
                            .message
                            .contains("live Main attempt could not be verified"),
                        "{}",
                        race_err.message
                    );
                    let after_race_text = std::fs::read_to_string(&events_path).unwrap();
                    let appended_rows: Vec<Value> = after_race_text
                        .lines()
                        .skip(before_race_lines)
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                    assert_eq!(appended_rows.len(), 2);
                    assert!(appended_rows.iter().any(|row| {
                        row["attempt_id"] == MAIN
                            && row["event_type"] == "attempt.state_changed"
                            && row["to_state"] == "abandoned"
                    }));
                    assert!(
                        !appended_rows
                            .iter()
                            .any(|row| row["actor"]["name"] == "panel-native-result")
                    );
                    let after_race_bytes = std::fs::read(&events_path).unwrap();
                    let unknown_transport = match text(&unknown_before["refs"], "native_transport")
                    {
                        "sending" => "sending",
                        "unknown" => "unknown",
                        other => panic!("unexpected unknown transport: {other}"),
                    };
                    let annotate_err = native_assignment::annotate_with_binding(
                        &race_panel,
                        text(&unknown, "attempt_id"),
                        text(&unknown_before["refs"], "native_intent_sha256"),
                        text(&unknown_before["refs"], "native_binding_sha256"),
                        unknown_transport,
                        "native-annotate-after-main-terminal",
                        json!({"native_transport": "unknown"}),
                    )
                    .unwrap_err();
                    assert!(
                        annotate_err
                            .message
                            .contains("live Main attempt could not be verified"),
                        "{}",
                        annotate_err.message
                    );
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_race_bytes);
                    let result_annotate_err = native_result::annotate(
                        &race_panel,
                        &unknown_before,
                        "native-result-annotate-after-main-terminal",
                        json!({"native_resolution": "reviewed"}),
                    )
                    .unwrap_err();
                    assert!(
                        result_annotate_err
                            .message
                            .contains("live Main attempt could not be verified"),
                        "{}",
                        result_annotate_err.message
                    );
                    assert_eq!(std::fs::read(&events_path).unwrap(), after_race_bytes);
                }
                if scenario == 2 {
                    // A canonical operation cancellation after buffering fences paste
                    // and Enter, even when the task's lease and pane are unchanged.
                    spy.task_transition.replace(Some((
                        "set-buffer",
                        Box::new(|spy| {
                            let output = oms_command_operations::run(
                                oms_command_operations::App::Events,
                                &commands::strings(&["list", "--json"]),
                                &spy.repo,
                                &spy.stop,
                            );
                            assert_eq!(output.0, 0);
                            let projection: Value = serde_json::from_slice(&output.1).unwrap();
                            let attempt = crate::rows(&projection)
                                .iter()
                                .find(|a| a["task_id"] == "native-four")
                                .unwrap();
                            let output = oms_command_operations::run(
                                oms_command_operations::App::Events,
                                &commands::strings(&[
                                    "transition",
                                    "--attempt",
                                    text(attempt, "attempt_id"),
                                    "--state",
                                    "cancelled",
                                    "--idempotency-key",
                                    "fixture-cancel",
                                ]),
                                &spy.repo,
                                &spy.stop,
                            );
                            assert_eq!(output.0, 0, "{}", String::from_utf8_lossy(&output.2));
                        }),
                    )));
                    spy.calls.borrow_mut().clear();
                    let cancelled = send("native-four", "fourth assignment").unwrap();
                    assert_eq!(cancelled.0, 75);
                    assert_eq!(spy.calls_named("set-buffer").len(), 1);
                    assert!(spy.calls_named("paste-buffer").is_empty());
                    assert!(spy.calls_named("send-keys").is_empty());
                    spy.calls.borrow_mut().clear();
                    let cancelled_retry = send("native-four", "fourth assignment").unwrap();
                    assert_eq!(cancelled_retry.0, 75);
                    assert!(spy.mutating_calls().is_empty());
                    let cancelled: Value = serde_json::from_slice(&cancelled.1).unwrap();
                    assert!(retain_return(&cancelled, "native-four", false).is_err());
                    assert!(spy.mutating_calls().is_empty());
                }
                if scenario == 3 {
                    // A pre-existing draft is a proven no-input prefix. Its intent
                    // remains reusable, and becoming idle submits the same claim once.
                    spy.screens
                        .borrow_mut()
                        .insert("%4".into(), "› preserved draft".into());
                    assert!(
                        send("native-five", "fifth assignment")
                            .unwrap_err()
                            .message
                            .contains("busy")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    let prefix = oms_command_operations::run(
                        oms_command_operations::App::Events,
                        &commands::strings(&["list", "--json"]),
                        &repo,
                        &spy.stop,
                    );
                    assert_eq!(prefix.0, 0);
                    let prefix: Value = serde_json::from_slice(&prefix.1).unwrap();
                    let prefix = crate::rows(&prefix)
                        .iter()
                        .find(|a| a["task_id"] == "native-five")
                        .unwrap();
                    assert_eq!(prefix["refs"]["native_transport"], "intent");
                    let mut changed_host = host.clone();
                    changed_host
                        .owners
                        .insert(2001, owner_entry(2001, "replaced-lead", uid));
                    assert!(
                        run_with_host(
                            &repo,
                            "codex",
                            &options("native-five", "fifth assignment"),
                            &spy,
                            &spy.stop,
                            &changed_host
                        )
                        .unwrap_err()
                        .message
                        .contains("custody")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    spy.screens
                        .borrow_mut()
                        .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
                    let resumed = send("native-five", "fifth assignment").unwrap();
                    assert_eq!(resumed.0, 0);
                    let resumed: Value = serde_json::from_slice(&resumed.1).unwrap();
                    assert_eq!(resumed["attempt_id"], prefix["attempt_id"]);
                    assert_eq!(spy.calls_named("send-keys").len(), 1);
                    let task: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-five"])).unwrap();
                    assert_eq!(task["lease_epoch"], 1);
                }
                if scenario == 4 {
                    // Lease changes after paste fence Enter, and a new claim cannot
                    // silently turn the unresolved old assignment into a new dispatch.
                    spy.task_transition.replace(Some((
                        "paste-buffer",
                        Box::new(|spy| {
                            let result = oms_command_plan::run(
                                &commands::strings(&["release", "--id", "native-three"]),
                                &spy.repo,
                                &spy.stop,
                            );
                            assert_eq!(result.0, 0, "{}", String::from_utf8_lossy(&result.2));
                        }),
                    )));
                    spy.calls.borrow_mut().clear();
                    let stale = send("native-three", "third assignment").unwrap();
                    assert_eq!(stale.0, 75);
                    assert_eq!(spy.calls_named("paste-buffer").len(), 1);
                    assert!(spy.calls_named("send-keys").is_empty());
                    spy.calls.borrow_mut().clear();
                    assert!(
                        send("native-three", "third assignment")
                            .unwrap_err()
                            .message
                            .contains("unresolved task")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    let stale: Value = serde_json::from_slice(&stale.1).unwrap();
                    assert!(retain_return(&stale, "native-three", false).is_err());
                    assert!(spy.mutating_calls().is_empty());
                }
                if scenario == 5 {
                    // Bound Proof::Turn withholds Enter on an opaque placeholder after paste and never resends.
                    spy.calls.borrow_mut().clear();
                    spy.cursor_x.set(2);
                    spy.screens
                        .borrow_mut()
                        .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
                    spy.task_transition.replace(Some((
                        "paste-buffer",
                        Box::new(|spy| {
                            spy.cursor_x.set(31);
                            spy.screens
                                .borrow_mut()
                                .insert("%4".into(), format!("{CODEX_OPAQUE_PASTE}{CODEX_STATUS}"));
                        }),
                    )));
                    let opaque = send("native-six", "sixth assignment").unwrap();
                    assert_eq!(opaque.0, 75);
                    let opaque: Value = serde_json::from_slice(&opaque.1).unwrap();
                    assert_eq!(opaque["status"], "unknown");
                    assert!(
                        opaque["reason"]
                            .as_str()
                            .is_some_and(|r| r.contains("Enter was withheld after pasting"))
                    );
                    assert_eq!(spy.calls_named("paste-buffer").len(), 1);
                    assert!(spy.calls_named("send-keys").is_empty());
                    spy.calls.borrow_mut().clear();
                    let opaque_retry = send("native-six", "sixth assignment").unwrap();
                    assert_eq!(opaque_retry.0, 75);
                    let opaque_retry: Value = serde_json::from_slice(&opaque_retry.1).unwrap();
                    assert_eq!(opaque_retry["status"], "unknown");
                    assert_eq!(opaque_retry["attempt_id"], opaque["attempt_id"]);
                    assert_eq!(opaque_retry["reused"], true);
                    assert!(spy.mutating_calls().is_empty());
                }
                if scenario == 6 {
                    let first = send("native-one", "Cancellation boundary").unwrap();
                    let first: Value = serde_json::from_slice(&first.1).unwrap();
                    let returned = retain_return(&first, "native-one", true).unwrap();
                    let attempt = text(&first, "attempt_id");
                    let sha = text(&returned["result"], "sha256");
                    let options = Options::parse(&commands::strings(&[
                        "--native-reconcile",
                        attempt,
                        "--result-sha256",
                        sha,
                        "--decision",
                        "reviewed",
                        "--room",
                        ROOM,
                    ]))
                    .unwrap();
                    run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
                    let reviewed: Value =
                        serde_json::from_slice(&plan(&["show", "--id", "native-one"])).unwrap();
                    assert_eq!(reviewed["review_evidence"]["schema"], 1);
                    // Independent operation cancellation can precede canonical
                    // settlement. Retirement must retain that terminal state and
                    // still record its intent before releasing Room membership.
                    let cancelled = oms_command_operations::run(
                        oms_command_operations::App::Events,
                        &commands::strings(&[
                            "transition",
                            "--attempt",
                            attempt,
                            "--state",
                            "cancelled",
                            "--idempotency-key",
                            "fixture-independent-native-cancel",
                        ]),
                        &repo,
                        &spy.stop,
                    );
                    assert_eq!(cancelled.0, 0, "{}", String::from_utf8_lossy(&cancelled.2));
                    let lease = text(&reviewed, "lease_id");
                    plan(&[
                        "block",
                        "--id",
                        "native-one",
                        "--lease-id",
                        lease,
                        "--reason",
                        "owner cancellation",
                    ]);
                    let blocked: Value =
                        serde_json::from_slice(&plan(&["evidence-snapshot", "--id", "native-one"]))
                            .unwrap();
                    assert_eq!(blocked["review_evidence"], reviewed["review_evidence"]);
                    let decision_path = ".oms/artifacts/native-cancel-decision.md";
                    let decision = b"Owner cancelled the reviewed candidate\n";
                    std::fs::write(repo.join(decision_path), decision).unwrap();
                    let plan_sha = oms_panel_core::digest(
                        &std::fs::read(repo.join(".oms/plan/tasks.json")).unwrap(),
                    );
                    plan(&[
                        "cancel",
                        "--id",
                        "native-one",
                        "--operator-consent",
                        "cancel",
                        "--expected-state",
                        "blocked",
                        "--expected-lease-id",
                        lease,
                        "--expected-plan-sha256",
                        &plan_sha,
                        "--expected-task-sha256",
                        text(&blocked, "task_sha256"),
                        "--decision-artifact",
                        decision_path,
                        "--expected-decision-artifact-sha256",
                        &oms_panel_core::digest(decision),
                        "--reason",
                        "owner cancellation",
                    ]);
                    let settled: Value =
                        serde_json::from_slice(&plan(&["evidence-snapshot", "--id", "native-one"]))
                            .unwrap();
                    assert_eq!(settled["state"], "cancelled");
                    assert_eq!(settled["review_evidence"], reviewed["review_evidence"]);
                    spy.task_snapshot_override
                        .replace(Some(json!({"id":"native-one","lease_epoch":2})));
                    assert!(
                        run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).is_err()
                    );
                    spy.task_snapshot_override.replace(None);
                    spy.task_transition.replace(Some((
                        "room-leave-recorded",
                        Box::new(|spy| spy.stop.store(true, Ordering::Release)),
                    )));
                    assert!(
                        run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).is_err()
                    );
                    spy.stop.store(false, Ordering::Release);
                    let retiring =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(retiring["state"], "cancelled");
                    assert_eq!(retiring["terminal"], true);
                    assert_eq!(retiring["refs"]["native_resolution"], "retiring");
                    assert_eq!(retiring["refs"]["native_retired_plan_state"], "cancelled");
                    assert_eq!(retiring["refs"]["native_result_sha256"], sha);
                    let room = commands::query(
                        &spy,
                        NativeCommand::Room,
                        &repo,
                        &["show", "--id", ROOM, "--exact", "--json"],
                        &spy.stop,
                    )
                    .unwrap();
                    assert!(
                        crate::rows(&room["participants"])
                            .iter()
                            .any(|p| p["participant"] == attempt && p["joined"] == false)
                    );
                    let retired =
                        run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
                    let retired: Value = serde_json::from_slice(&retired.1).unwrap();
                    assert_eq!(retired["status"], "retired");
                    assert_eq!(retired["plan_state"], "cancelled");
                    let operation =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(operation["state"], "cancelled");
                    assert_eq!(operation["refs"]["native_resolution"], "retired");
                    assert_eq!(operation["refs"]["native_result_sha256"], sha);
                    let before = std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
                    run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
                    assert_eq!(
                        std::fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap(),
                        before
                    );
                    assert!(spy.calls_named("kill-pane").is_empty());
                }
                if (7..=10).contains(&scenario) {
                    // A round-1 crash can leave the packet written (7) or missing
                    // (8 through 10), with no binding record in either case.
                    native_assignment::CRASH_AFTER_BINDING_WRITE.with(|crash| crash.set(true));
                    assert!(
                        send("native-one", "Crash window")
                            .unwrap_err()
                            .message
                            .contains("test crash")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    let events = oms_command_operations::run(
                        oms_command_operations::App::Events,
                        &commands::strings(&["list", "--json"]),
                        &repo,
                        &spy.stop,
                    );
                    assert_eq!(events.0, 0);
                    let events: Value = serde_json::from_slice(&events.1).unwrap();
                    let crashed = crate::rows(&events)
                        .iter()
                        .find(|a| a["task_id"] == "native-one")
                        .unwrap()
                        .clone();
                    let attempt = text(&crashed, "attempt_id");
                    assert_eq!(crashed["refs"]["native_transport"], "intent");
                    assert!(
                        crashed["refs"]["native_binding_sha256"]
                            .as_str()
                            .is_none_or(str::is_empty)
                    );
                    let artifact =
                        format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
                    let path = repo.join(&artifact);
                    let written = std::fs::read(&path).unwrap();
                    let intent = text(&crashed["refs"], "native_intent_sha256");
                    let mut old_packet: Value = serde_json::from_slice(&written).unwrap();
                    let old_fields = [
                        "schema",
                        "kind",
                        "task",
                        "parent_attempt_id",
                        "room",
                        "provider",
                        "session",
                        "main_pane",
                        "lead_pane",
                        "account",
                        "account_verification",
                        "checkout",
                        "room_message_id",
                        "payload_sha256",
                        "payload",
                        "round",
                        "brief",
                        "result_contract",
                    ];
                    let old = old_packet.as_object_mut().unwrap();
                    old.shift_remove("attempt_id");
                    assert!(old.keys().map(String::as_str).eq(old_fields));
                    assert_eq!(
                        oms_panel_core::digest(&serde_json::to_vec(&old_packet).unwrap()),
                        intent
                    );
                    let events_path = repo.join(".oms/lifecycle/events.jsonl");
                    let before = std::fs::read(&events_path).unwrap();
                    if scenario == 7 {
                        let error = retire_unsent(attempt, intent).unwrap_err();
                        assert!(
                            error.message.contains("missing binding path"),
                            "{}",
                            error.message
                        );
                        let mut changed: Value = serde_json::from_slice(&written).unwrap();
                        changed["room_message_id"] = format!("msg-{}", "a".repeat(32)).into();
                        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
                        let mismatch = send("native-one", "Crash window").unwrap_err();
                        assert!(
                            mismatch
                                .message
                                .contains("does not match its recorded intent")
                        );
                        assert!(spy.mutating_calls().is_empty());
                        std::fs::write(&path, &written).unwrap();
                        assert_eq!(std::fs::read(&events_path).unwrap(), before);
                    } else {
                        std::fs::remove_file(&path).unwrap();
                    }
                    // The crashed row still reserves the Lead, even unreadable.
                    let other = send("native-two", "Other task").unwrap_err().message;
                    assert!(
                        other.contains("still owns unresolved task native-one"),
                        "{other}"
                    );
                    if scenario >= 8 {
                        // A drifted packet cannot adopt the crashed intent.
                        let drifted = send("native-one", "Changed brief").unwrap_err().message;
                        assert!(drifted.contains("different start request"), "{drifted}");
                        assert!(!repo.join(&artifact).exists());
                    }
                    assert!(spy.mutating_calls().is_empty());
                    if scenario == 9 {
                        let bad_digest = retire_unsent(attempt, &"0".repeat(64)).unwrap_err();
                        assert!(
                            bad_digest.message.contains("exact typed assignment"),
                            "{}",
                            bad_digest.message
                        );
                        std::fs::write(&path, b"").unwrap();
                        let empty = retire_unsent(attempt, intent).unwrap_err();
                        assert!(
                            empty.message.contains("missing binding path"),
                            "{}",
                            empty.message
                        );
                        std::fs::remove_file(&path).unwrap();
                        std::os::unix::fs::symlink(path.with_extension("absent"), &path).unwrap();
                        let symlink = retire_unsent(attempt, intent).unwrap_err();
                        assert!(
                            symlink.message.contains("missing binding path"),
                            "{}",
                            symlink.message
                        );
                        std::fs::remove_file(&path).unwrap();
                        assert_eq!(std::fs::read(&events_path).unwrap(), before);

                        let plan_path = repo.join(".oms/plan/tasks.json");
                        let plan_before = std::fs::read(&plan_path).unwrap();
                        let retired = retire_unsent(attempt, intent).unwrap();
                        let retired: Value = serde_json::from_slice(&retired.1).unwrap();
                        assert_eq!(retired["status"], "retired-unsent");
                        assert_eq!(retired["reused"], false);
                        assert_eq!(retired["plan_changed"], false);
                        assert_eq!(std::fs::read(&plan_path).unwrap(), plan_before);
                        let operation =
                            native_assignment::current_operation(&spy.load(), attempt).unwrap();
                        assert_eq!(operation["state"], "cancelled");
                        assert_eq!(operation["sequence"], 2);
                        assert_eq!(operation["refs"]["native_resolution"], "retired");
                        let same_lease = send("native-one", "Crash window").unwrap_err();
                        assert!(
                            same_lease.message.contains("release its exact Plan lease"),
                            "{}",
                            same_lease.message
                        );
                        assert!(!path.exists());
                        assert!(spy.mutating_calls().is_empty());

                        let next = send("native-two", "Other task").unwrap();
                        assert_eq!(next.0, 0);
                        let next: Value = serde_json::from_slice(&next.1).unwrap();
                        assert_eq!(next["status"], "submitted");
                        assert_ne!(next["attempt_id"], attempt);
                        assert_eq!(spy.calls_named("send-keys").len(), 1);

                        plan(&[
                            "release",
                            "--id",
                            "native-one",
                            "--lease-id",
                            text(&crashed["refs"], "lease_id"),
                        ]);
                        let before_replay = std::fs::read(&events_path).unwrap();
                        let replay = retire_unsent(attempt, intent).unwrap();
                        let replay: Value = serde_json::from_slice(&replay.1).unwrap();
                        assert_eq!(replay["reused"], true);
                        assert_eq!(std::fs::read(&events_path).unwrap(), before_replay);
                        assert_eq!(spy.calls_named("send-keys").len(), 1);
                        continue;
                    }
                    if scenario == 10 {
                        let cancelled = oms_command_operations::run(
                            oms_command_operations::App::Events,
                            &commands::strings(&[
                                "transition",
                                "--attempt",
                                attempt,
                                "--state",
                                "cancelled",
                                "--idempotency-key",
                                "fixture-unsent-cancel",
                            ]),
                            &repo,
                            &spy.stop,
                        );
                        assert_eq!(cancelled.0, 0, "{}", String::from_utf8_lossy(&cancelled.2));
                        let after_cancel = std::fs::read(&events_path).unwrap();
                        assert!(send("native-one", "Crash window").is_err());
                        assert!(!path.exists(), "terminal intent recreated a binding");
                        assert_eq!(std::fs::read(&events_path).unwrap(), after_cancel);
                        assert!(spy.mutating_calls().is_empty());
                        continue;
                    }
                    if scenario == 8 {
                        std::fs::write(&path, b"").unwrap();
                        assert!(
                            send("native-one", "Crash window").is_err(),
                            "empty binding was reused"
                        );
                        assert_eq!(std::fs::read(&path).unwrap(), b"");
                        assert_eq!(std::fs::read(&events_path).unwrap(), before);
                        assert!(spy.mutating_calls().is_empty());
                        std::fs::remove_file(&path).unwrap();
                    }
                    let recovered = send("native-one", "Crash window").unwrap();
                    assert_eq!(recovered.0, 0);
                    let recovered: Value = serde_json::from_slice(&recovered.1).unwrap();
                    assert_eq!(recovered["status"], "submitted");
                    assert_eq!(recovered["attempt_id"], attempt);
                    assert_eq!(recovered["binding"], artifact.as_str());
                    assert_eq!(spy.calls_named("send-keys").len(), 1);
                    let bytes = std::fs::read(repo.join(&artifact)).unwrap();
                    assert_eq!(bytes, written);
                    let operation =
                        native_assignment::current_operation(&spy.load(), attempt).unwrap();
                    assert_eq!(operation["refs"]["native_binding"], artifact.as_str());
                    assert_eq!(
                        operation["refs"]["native_binding_sha256"],
                        oms_panel_core::digest(&bytes)
                    );
                    assert_eq!(operation["refs"]["native_transport"], "submitted");
                    let returned = retain_return(&recovered, "native-one", false).unwrap();
                    assert!(
                        returned["result"]["sha256"].as_str().is_some(),
                        "{returned}"
                    );
                }
                if scenario == 11 {
                    use oms_command_operations as events;
                    use oms_command_operations::value::V;

                    plan(&["claim", "--id", "native-one", "--provider", "codex"]);
                    let panel = spy.load();
                    let task = native_assignment::task(&panel, "native-one").unwrap();
                    let custody = panel.codex_assignment_custody(&host).unwrap();
                    let (_, _, verification) = binding_summary(custody.binding.as_ref());
                    let pane = |p: &PaneIdentity| json!({"id":p.id,"window":p.window,"pid":p.pid,"birth":p.birth});
                    let brief = "Inspect the legacy assignment";
                    let payload = format!(
                        "{} Turn {}. Task: {brief}",
                        crate::native_children::TASK_PREFIX,
                        "b".repeat(20)
                    );
                    let payload_digest = oms_panel_core::digest(payload.as_bytes());
                    let mut packet = json!({"schema":1,"kind":"oms-native-assignment",
                        "provider":"codex","parent_attempt_id":MAIN,"room":ROOM,"task":task,
                        "session":panel.session_proof,"main_pane":pane(&custody.main),
                        "lead_pane":pane(&custody.lead),"account":custody.binding,
                        "account_verification":verification,
                        "checkout":native_assignment::checkout(&panel, &panel.leads["codex"]).unwrap(),
                        "room_message_id":format!("msg-{}", "c".repeat(32)),
                        "payload":payload,"payload_sha256":payload_digest,
                        "result_contract":"preserve task, lease, and attempt binding"});
                    let intent = oms_panel_core::digest(&serde_json::to_vec(&packet).unwrap());
                    let refs = json!({"native_assignment_schema":"1","native_transport":"intent",
                        "native_intent_sha256":intent,"native_payload_sha256":payload_digest,
                        "panel_room_id":ROOM,"panel_owner":"codex","panel_native_scheduling":"unmanaged",
                        "plan_id":task["plan_id"],"lease_id":task["lease_id"]});
                    let attempt = events::create_attempt(
                        &repo,
                        &events::StartRequest {
                            provider: "codex".into(),
                            tool: "panel-native-assignment".into(),
                            task_id: "native-one".into(),
                            parent_attempt_id: MAIN.into(),
                            idempotency_key: "native-legacy-fixture".into(),
                            refs: V::parse(&serde_json::to_vec(&refs).unwrap()).unwrap(),
                            ..Default::default()
                        },
                        &spy.stop,
                    )
                    .unwrap();
                    packet["attempt_id"] = attempt.as_str().into();
                    let raw = serde_json::to_vec(&packet).unwrap();
                    let sha = oms_panel_core::digest(&raw);
                    let name = format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
                    let write = |bytes: &[u8]| {
                        let store = Store::open(&repo).unwrap();
                        let tx = store
                            .transaction("legacy-native-fixture", Duration::from_secs(1))
                            .unwrap();
                        tx.write(
                            Path::new(&name),
                            bytes,
                            false,
                            native_assignment::MAX_BINDING,
                        )
                        .unwrap();
                    };
                    let mut changed = packet;
                    changed["payload"] = format!("Changed intent. Task: {brief}").into();
                    write(&serde_json::to_vec(&changed).unwrap());
                    assert!(
                        send("native-one", brief)
                            .unwrap_err()
                            .message
                            .contains("intent digest")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    write(&raw);
                    let adopt = Options::parse(&commands::strings(&[
                        "--native-adopt",
                        &attempt,
                        "--assignment-sha256",
                        &sha,
                        "--room",
                        ROOM,
                    ]))
                    .unwrap();
                    assert!(
                        run_with_host(&repo, "codex", &adopt, &spy, &spy.stop, &host)
                            .unwrap_err()
                            .message
                            .contains("requires a typed assignment")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    let assigned = send("native-one", brief).unwrap();
                    let assigned: Value = serde_json::from_slice(&assigned.1).unwrap();
                    assert_eq!(assigned["status"], "submitted");
                    assert_eq!(assigned["binding"], name);
                    assert_eq!(spy.calls_named("send-keys").len(), 1);
                    spy.calls.borrow_mut().clear();
                    let replay = send("native-one", brief).unwrap();
                    let replay: Value = serde_json::from_slice(&replay.1).unwrap();
                    assert_eq!(replay["reused"], true);
                    assert_eq!(replay["attempt_id"], attempt);
                    assert!(
                        send("native-one", "Change the original scope")
                            .unwrap_err()
                            .message
                            .contains("brief changed")
                    );
                    assert!(
                        send("native-two", "Try a second assignment")
                            .unwrap_err()
                            .message
                            .contains("unresolved task")
                    );
                    assert!(spy.mutating_calls().is_empty());
                    spy.task_snapshot_override.replace(Some(json!({
                        "id":"native-one","state":"cancelled","completion_verified":true
                    })));
                    let next = send("native-two", "Start the next assignment").unwrap();
                    let next: Value = serde_json::from_slice(&next.1).unwrap();
                    assert_eq!(next["status"], "submitted");
                    let retired = native_assignment::current_operation(&panel, &attempt).unwrap();
                    assert_eq!(retired["state"], "cancelled");
                    assert_eq!(retired["refs"]["native_resolution"], "retired");
                    let room = commands::query(
                        &spy,
                        NativeCommand::Room,
                        &repo,
                        &["show", "--id", ROOM, "--exact", "--json"],
                        &spy.stop,
                    )
                    .unwrap();
                    assert!(crate::rows(&room["participants"]).iter().any(|member| {
                        member["participant"] == attempt && member["joined"] == false
                    }));
                }
            }
        }

        #[test]
        fn codex_assignment_reuses_attempt_cache_and_refreshes_on_append_or_replacement() {
            use std::os::unix::fs::MetadataExt;
            const CASE: &str = "OMS_R23_V2_CACHE_TEST";
            const TEST: &str = "orchestrator::tests::account_tests::codex_assignment_reuses_attempt_cache_and_refreshes_on_append_or_replacement";
            let tmp = TempDir::new("orch-r23-cache");
            if std::env::var_os(CASE).is_none() {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env_clear()
                    .env("HOME", &tmp.0)
                    .env(CASE, "1")
                    .env("TMUX_PANE", "%1")
                    .env("OMS_PANEL_MAIN_ATTEMPT", MAIN)
                    .env("OMS_ROOM_PARTICIPANT", MAIN)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                print!("{stdout}");
                eprint!("{stderr}");
                assert!(
                    output.status.success() && stdout.contains("1 passed"),
                    "stdout:\n{stdout}\nstderr:\n{stderr}"
                );
                return;
            }
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-a"));
            let uid = rustix::process::getuid().as_raw();
            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![("codex", vec![("acct-a", codex_a)])]));
            let spy = SpyPanel::new(repo.clone());
            spy.allow_board_queries.set(true);
            spy.real_events.set(true);
            let base_ledger = write_synthetic_ledger(&repo, 4096);
            let events_path = repo.join(".oms/lifecycle/events.jsonl");
            assert!(base_ledger.lines().count() >= 4096);
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "fixture-main", uid));
            host.owners.insert(2001, owner_entry(2001, "p2001", uid));

            assert!(spy.load().require_main(Some("codex")).is_ok());
            spy.load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();

            let options = Options::parse(&commands::strings(&[
                "--orchestrator",
                "codex",
                "--account",
                "acct-a",
                "--task",
                "synthetic cache check",
                "--room",
                ROOM,
            ]))
            .unwrap();
            let reset_spy = || {
                spy.calls.borrow_mut().clear();
                spy.room_queries.borrow_mut().clear();
                spy.event_lists.set(0);
                spy.assignment_events.set(0);
                spy.cursor_x.set(2);
                spy.screens
                    .borrow_mut()
                    .insert("%4".into(), format!("{CODEX_EMPTY}{CODEX_STATUS}"));
            };

            // 1. Unchanged >=4096-row synthetic ledger: 1 cold replay across require_main + 3 fences.
            reset_spy();
            let start = Instant::now();
            run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
            let unchanged_elapsed = start.elapsed();
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.assignment_events.get(), 1);
            assert_eq!(*spy.room_queries.borrow(), [true; 4]);
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert_eq!(spy.calls_named("send-keys").len(), 1);
            println!(
                "SYNTHETIC_LEDGER_UNCHANGED: rows={} event_lists={} assignment_events={} room_queries={} elapsed_ms={:.3}",
                base_ledger.lines().count(),
                spy.event_lists.get(),
                spy.assignment_events.get(),
                spy.room_queries.borrow().len(),
                unchanged_elapsed.as_secs_f64() * 1000.0
            );

            // 2. Shared AttemptCache across calls: 0 replays when unchanged, +1 replay after benign append.
            reset_spy();
            let shared_cache = AttemptCache::default();
            let panel = spy.load();
            panel
                .execute_with_host_cached(
                    "codex",
                    None,
                    Some("cached one"),
                    LeadLaunch::default(),
                    LeadExecution {
                        host: &host,
                        attempts: &shared_cache,
                        transaction: None,
                    },
                )
                .unwrap();
            assert_eq!(spy.event_lists.get(), 1);
            reset_spy();
            panel
                .execute_with_host_cached(
                    "codex",
                    None,
                    Some("cached two"),
                    LeadLaunch::default(),
                    LeadExecution {
                        host: &host,
                        attempts: &shared_cache,
                        transaction: None,
                    },
                )
                .unwrap();
            assert_eq!(spy.event_lists.get(), 0);
            append_lifecycle_row(
                &repo,
                &benign_heartbeat_row("att_a0000000000000000000000000000000", 2, 1),
            );
            panel
                .execute_with_host_cached(
                    "codex",
                    None,
                    Some("cached three"),
                    LeadLaunch::default(),
                    LeadExecution {
                        host: &host,
                        attempts: &shared_cache,
                        transaction: None,
                    },
                )
                .unwrap();
            assert_eq!(spy.event_lists.get(), 1);

            // 3. run_with_host after benign append refreshes once cleanly and succeeds.
            append_lifecycle_row(
                &repo,
                &benign_heartbeat_row("att_a0000000000000000000000000000000", 3, 2),
            );
            reset_spy();
            let start = Instant::now();
            run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap();
            let append_elapsed = start.elapsed();
            assert_eq!(spy.event_lists.get(), 1);
            assert_eq!(spy.assignment_events.get(), 1);
            assert_eq!(spy.calls_named("send-keys").len(), 1);
            println!(
                "SYNTHETIC_LEDGER_BENIGN_APPEND: event_lists={} assignment_events={} elapsed_ms={:.3}",
                spy.event_lists.get(),
                spy.assignment_events.get(),
                append_elapsed.as_secs_f64() * 1000.0
            );

            // 4. Append terminalizing Main right after capture-pane (pre-set-buffer): 0 mutating calls.
            std::fs::write(&events_path, &base_ledger).unwrap();
            reset_spy();
            spy.task_transition.replace(Some((
                "capture-pane",
                Box::new(|spy| {
                    append_lifecycle_row(&spy.repo, &main_failed_row(4, 10));
                }),
            )));
            let err = run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap_err();
            assert!(err.message.contains("live Main attempt"));
            assert!(!err.publication_uncertain);
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(spy.event_lists.get(), 2);

            // 5. Append terminalizing Main right after paste-buffer: withholds Enter, uncertain=true.
            std::fs::write(&events_path, &base_ledger).unwrap();
            reset_spy();
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(|spy| {
                    append_lifecycle_row(&spy.repo, &main_failed_row(4, 11));
                }),
            )));
            let err = run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap_err();
            assert!(err.publication_uncertain);
            assert!(err.message.contains("live Main attempt"));
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            assert_eq!(spy.event_lists.get(), 2);

            // 6. Atomic rename replacement with IDENTICAL byte size & new inode:
            //    terminalize Main right after capture-pane AND right after paste-buffer.
            let same_size_bytes = base_ledger
                .replacen("\"to_state\":\"working\"", "\"to_state\":\"failed\"", 1)
                .replacen("\"name\":\"agent-events\"", "\"name\":\"agent-events1\"", 1);
            assert_eq!(same_size_bytes.len(), base_ledger.len());
            let staging = repo.join(".oms/lifecycle/events.jsonl.tmp");

            std::fs::write(&events_path, &base_ledger).unwrap();
            let pre_meta = std::fs::metadata(&events_path).unwrap();
            reset_spy();
            let stage_a = staging.clone();
            let bytes_a = same_size_bytes.clone();
            spy.task_transition.replace(Some((
                "capture-pane",
                Box::new(move |_| {
                    std::fs::write(&stage_a, &bytes_a).unwrap();
                    std::fs::rename(&stage_a, stage_a.with_file_name("events.jsonl")).unwrap();
                }),
            )));
            let err = run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap_err();
            let post_meta = std::fs::metadata(&events_path).unwrap();
            assert_eq!(post_meta.size(), pre_meta.size());
            assert_ne!(post_meta.ino(), pre_meta.ino());
            assert!(err.message.contains("live Main attempt"));
            assert!(!err.publication_uncertain);
            assert!(spy.mutating_calls().is_empty());
            assert_eq!(spy.event_lists.get(), 2);

            std::fs::write(&events_path, &base_ledger).unwrap();
            let pre_meta = std::fs::metadata(&events_path).unwrap();
            reset_spy();
            let stage_b = staging;
            let bytes_b = same_size_bytes;
            spy.task_transition.replace(Some((
                "paste-buffer",
                Box::new(move |_| {
                    std::fs::write(&stage_b, &bytes_b).unwrap();
                    std::fs::rename(&stage_b, stage_b.with_file_name("events.jsonl")).unwrap();
                }),
            )));
            let err = run_with_host(&repo, "codex", &options, &spy, &spy.stop, &host).unwrap_err();
            let post_meta = std::fs::metadata(&events_path).unwrap();
            assert_eq!(post_meta.size(), pre_meta.size());
            assert_ne!(post_meta.ino(), pre_meta.ino());
            assert!(err.publication_uncertain);
            assert!(err.message.contains("live Main attempt"));
            assert_eq!(spy.calls_named("paste-buffer").len(), 1);
            assert!(spy.calls_named("send-keys").is_empty());
            assert_eq!(spy.event_lists.get(), 2);
        }

        #[test]
        fn observe_tracks_verification_focus_staleness_rollout_suppression_and_quota() {
            let tmp = TempDir::new("orch-observe");
            let repo = private_dir(&tmp.0.join("repo"));
            let repo_other = private_dir(&tmp.0.join("repo-other"));
            let state = private_dir(&tmp.0.join("state"));
            let codex_a = private_dir(&tmp.0.join("codex-a"));
            let codex_b = private_dir(&tmp.0.join("codex-b"));
            let claude_work = private_dir(&tmp.0.join("claude-work"));
            let uid = rustix::process::getuid().as_raw();

            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![
                (
                    "codex",
                    vec![("acct-a", codex_a.clone()), ("acct-b", codex_b.clone())],
                ),
                ("claude", vec![("work", claude_work)]),
            ]));
            host.owners.insert(2001, owner_entry(2001, "b2001", uid));
            host.owners.insert(2002, owner_entry(2002, "b2002", uid));
            host.owners.insert(2003, owner_entry(2003, "b2003", uid));
            host.owners.insert(2004, owner_entry(2004, "b2004", uid));

            let spy = SpyPanel::new(repo.clone());
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "main-birth", uid));
            // Start pinned codex (acct-a -> %4) and pinned claude (work -> %5).
            spy.load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();
            spy.spawn_pid.set(2002);
            spy.load()
                .execute_with_host("claude", Some("work"), None, &host)
                .unwrap();
            // Add untagged antigravity lead (%6).
            spy.leads.borrow_mut().push(FakeLead {
                window: "@6".into(),
                pane_id: "%6".into(),
                pid: 2003,
                active: false,
                provider: "antigravity".into(),
            });

            // Before model row is observed, verified providers still format "@<acct>".
            let mut obs = Observations::default();
            let mut report = json!({});
            spy.load()
                .observe_with_host(&mut obs, &mut report, false, &host);
            assert_eq!(report["provider_models"]["codex"], "@acct-a");
            assert_eq!(report["provider_models"]["claude"], "@work");
            assert_eq!(
                report["provider_accounts"]["codex"],
                json!({"account": "acct-a", "pinned": true, "verification": "verified"})
            );
            assert_eq!(
                report["provider_accounts"]["claude"],
                json!({"account": "work", "pinned": true, "verification": "verified"})
            );
            assert_eq!(
                report["provider_accounts"]["antigravity"],
                json!({"account": null, "pinned": false, "verification": "unverified"})
            );

            // Set screens with model and claude /usage quota.
            spy.screens.borrow_mut().insert(
                "%4".into(),
                "  GPT-6.2-Sol high · Context 95% left · weekly 48% left".into(),
            );
            spy.screens.borrow_mut().insert(
                "%5".into(),
                "  Opus 4.5 | main | ctx [----] 10%\n  Settings   Status   Config   Usage   Stats\n\n  Current week (all models)\n  ███      37% used".into(),
            );
            spy.screens.borrow_mut().insert(
                "%6".into(),
                "Gemini 4 Hydrogen (High) (Antigravity ) | ctx 2% | main".into(),
            );

            // Models still refresh without a verified Room/Main snapshot.
            host.active_homes.insert("codex".into(), codex_b);
            let mut report = json!({
                "provider_status": {
                    "claude": {
                        "account_label": "personal",
                        "observed_at": 10.0,
                        "weekly_used": 12,
                        "accounts": {
                            "personal": { "weekly_used": 12 },
                            "work": { "weekly_used": null }
                        }
                    }
                }
            });
            spy.calls.borrow_mut().clear();
            spy.load()
                .observe_with_host(&mut obs, &mut report, true, &host);
            assert_eq!(report["provider_models"]["codex"], "GPT-6.2-Sol @acct-a");
            assert_eq!(report["provider_models"]["claude"], "Opus 4.5 @work");
            assert_eq!(
                report["provider_models"]["antigravity"],
                "Gemini 4 Hydrogen (High)"
            );
            // Without an accepted scope, there is no bound Codex rollout capture.
            assert!(
                !spy.calls_named("capture-pane")
                    .iter()
                    .any(|c| c.contains(&"-J".to_owned()) && c.contains(&"%4".to_owned()))
            );
            // An unattributed Claude dialog cannot update a pinned account.
            let claude = &report["provider_status"]["claude"];
            assert!(claude["accounts"]["work"]["weekly_used"].is_null());
            assert!(claude.get("weekly_used").is_none());
            assert!(claude.get("account_label").is_none());

            // The same ambient label alone does not prove ownership.
            let mut report_matching = json!({
                "provider_status": {
                    "claude": {
                        "account_label": "work",
                        "observed_at": 10.0,
                        "weekly_used": 12,
                        "accounts": { "work": { "weekly_used": null } }
                    }
                }
            });
            spy.load()
                .observe_with_host(&mut obs, &mut report_matching, false, &host);
            assert!(
                report_matching["provider_status"]["claude"]
                    .get("weekly_used")
                    .is_none()
            );
            assert!(
                report_matching["provider_status"]["claude"]
                    .get("account_label")
                    .is_none()
            );

            // With accepted scope, bound Codex inspection uses acct-a despite ambient acct-b.
            spy.calls.borrow_mut().clear();
            let mut report_match = accepted_codex_scope();
            spy.load().observe_with_claude_reader(
                &mut obs,
                &mut report_match,
                true,
                &host,
                |_, _, _| None,
            );
            assert_eq!(
                spy.calls_named("capture-pane")
                    .iter()
                    .any(|c| c.contains(&"-J".to_owned()) && c.contains(&"%4".to_owned())),
                std::fs::metadata(&codex_a)
                    .unwrap()
                    .created()
                    .is_ok_and(|birth| birth > std::time::UNIX_EPOCH)
            );
            // The Board's read fences Claude's child sighting with one fresh pane
            // census (AGY paints no children here); other reads take none.
            let generation = spy.load().observation_generation_with_host(&host).unwrap();
            let censuses = [None, Some(&generation)].map(|expected| {
                let panel = spy.load();
                spy.calls.borrow_mut().clear();
                panel.observe_reading(
                    &mut obs.clone(),
                    &mut accepted_codex_scope(),
                    true,
                    &host,
                    |_, _, _| None,
                    expected,
                );
                spy.calls_named("list-panes").len()
            });
            assert_eq!(censuses, [0, 1]);

            // Verify ChatPane captures bound_account and detects binding change.
            let pane_a = spy.load().chat_pane_with_host("codex", &host).unwrap();
            assert_eq!(
                pane_a.bound_account,
                Some(("acct-a".into(), account_binding::path_digest(&codex_a)))
            );

            // Focus / dock moves (pane.active & pane.window changes) preserve cached model & weekly!
            spy.screens.borrow_mut().clear();
            {
                let mut leads = spy.leads.borrow_mut();
                leads[0].active = true;
                leads[0].window = "@1".into();
                leads[1].active = true;
                leads[1].window = "@1".into();
            }
            let mut report_moved = json!({});
            spy.load()
                .observe_with_host(&mut obs, &mut report_moved, false, &host);
            assert_eq!(
                report_moved["provider_models"]["codex"],
                "GPT-6.2-Sol @acct-a"
            );
            assert_eq!(report_moved["provider_models"]["claude"], "Opus 4.5 @work");
            assert!(obs.panes["claude"].weekly.is_some());

            // Changing birth invalidates cached model & weekly & downgrades to unverified!
            host.owners
                .insert(2001, owner_entry(2001, "b2001-reused", uid));
            let mut report_birth = json!({});
            spy.load()
                .observe_with_host(&mut obs, &mut report_birth, false, &host);
            assert!(report_birth["provider_models"].get("codex").is_none());
            assert_eq!(
                report_birth["provider_accounts"]["codex"]["verification"],
                "unverified"
            );
            // Click verification also sees bound_account mismatch.
            let pane_reused = spy.load().chat_pane_with_host("codex", &host).unwrap();
            assert!(pane_reused != pane_a);
            assert_eq!(pane_reused.bound_account, None);

            // Restore birth, re-populate cache, then test pid/pane_id/room/repo staleness.
            host.owners.insert(2001, owner_entry(2001, "b2001", uid));
            spy.screens.borrow_mut().insert(
                "%4".into(),
                "  GPT-6.2-Sol high · Context 95% left · weekly 48% left".into(),
            );
            spy.load()
                .observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert_eq!(obs.panes["codex"].model.as_deref(), Some("GPT-6.2-Sol"));

            // Changing pid purges cached model.
            spy.leads.borrow_mut()[0].pid = 2004;
            spy.screens.borrow_mut().clear();
            let mut report_pid = json!({});
            spy.load()
                .observe_with_host(&mut obs, &mut report_pid, false, &host);
            assert!(obs.panes["codex"].model.is_none());
            assert!(report_pid["provider_models"].get("codex").is_none());

            // Changing repo purges cached claude model & weekly.
            assert!(obs.panes["claude"].weekly.is_some());
            assert_eq!(obs.panes["claude"].model.as_deref(), Some("Opus 4.5"));
            let other_panel = Panel::load_for_caller(&repo_other, ROOM, "%1", &spy, &spy.stop)
                .unwrap_or_else(|_| {
                    let mut panel = spy.load();
                    panel.repo = &repo_other;
                    panel
                });
            let mut report_repo = json!({});
            other_panel.observe_with_host(&mut obs, &mut report_repo, false, &host);
            assert!(obs.panes["claude"].model.is_none());
            assert!(obs.panes["claude"].weekly.is_none());
            assert_eq!(
                report_repo["provider_accounts"]["claude"]["verification"],
                "unverified"
            );

            // Restore claude model/weekly, then test pane_id, room, and main_attempt staleness.
            spy.screens.borrow_mut().insert(
                "%5".into(),
                "  Opus 4.5 | main | ctx [----] 10%\n  Settings   Status   Config   Usage   Stats\n\n  Current week (all models)\n  ███      37% used".into(),
            );
            spy.load()
                .observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert!(obs.panes["claude"].weekly.is_some());
            assert_eq!(obs.panes["claude"].model.as_deref(), Some("Opus 4.5"));

            // Changing room purges cache.
            spy.screens.borrow_mut().clear();
            let mut room_panel = spy.load();
            room_panel.room = "other-room";
            room_panel.observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert!(obs.panes["claude"].model.is_none());
            assert!(obs.panes["claude"].weekly.is_none());

            // Changing main_attempt purges cache.
            spy.screens
                .borrow_mut()
                .insert("%5".into(), "  Opus 4.5 | main | ctx [----] 10%".into());
            spy.load()
                .observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert_eq!(obs.panes["claude"].model.as_deref(), Some("Opus 4.5"));
            spy.screens.borrow_mut().clear();
            let mut attempt_panel = spy.load();
            attempt_panel.attempt = "att_22222222222222222222222222222222".into();
            attempt_panel.observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert!(obs.panes["claude"].model.is_none());

            // Changing pane_id purges cache.
            spy.screens
                .borrow_mut()
                .insert("%5".into(), "  Opus 4.5 | main | ctx [----] 10%".into());
            spy.load()
                .observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert_eq!(obs.panes["claude"].model.as_deref(), Some("Opus 4.5"));
            spy.screens.borrow_mut().clear();
            spy.leads.borrow_mut()[1].pane_id = "%55".into();
            spy.load()
                .observe_with_host(&mut obs, &mut json!({}), false, &host);
            assert!(obs.panes["claude"].model.is_none());
        }

        #[test]
        fn unpinned_fresh_codex_ranking_and_ambient_compatibility() {
            const CHILD: &str = "OMS_QUOTA_SOURCE_TEST";
            if std::env::var_os(CHILD).is_none() {
                let home = TempDir::new("quota-source-home");
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "orchestrator::tests::account_tests::unpinned_fresh_codex_ranking_and_ambient_compatibility",
                    ])
                    .env_clear()
                    .env("HOME", &home.0)
                    .env(CHILD, "1")
                    .output()
                    .unwrap();
                assert!(
                    child.status.success(),
                    "{}",
                    String::from_utf8_lossy(&child.stdout)
                );
                return;
            }

            struct QuotaReads;
            impl PeerExecutor for QuotaReads {
                fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                    panic!("quota fixture executed an unexpected process");
                }
            }
            impl Services for QuotaReads {
                fn command(
                    &self,
                    command: NativeCommand,
                    _: &Path,
                    args: &[OsString],
                    _: &AtomicBool,
                ) -> Result<CommandOutput> {
                    let value = match command {
                        NativeCommand::Events if args == ["list", "--json"] => json!([{
                            "attempt_id":MAIN, "provider":"codex", "tool":"panel-main",
                            "terminal":false, "state":"working",
                            "refs":{"panel_role":"main","panel_owner":"codex",
                                    "panel_harness":"codex","panel_room_id":ROOM}
                        }]),
                        NativeCommand::Room if args == ["show", "--id", ROOM, "--json"] => {
                            json!({"id":ROOM,"closed":false,"participants":[{
                                "participant":MAIN,"role":"main","provider":"codex","joined":true
                            }]})
                        }
                        _ => panic!("unexpected quota fixture read"),
                    };
                    Ok(out(serde_json::to_vec(&value).unwrap()))
                }
            }

            let tmp = TempDir::new("orch-unpinned");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            let codex_a = private_dir(&home.join(".codex"));
            let codex_b = private_dir(&tmp.0.join("codex-b"));
            let moved = private_dir(&tmp.0.join("replacement-home"));
            let uid = rustix::process::getuid().as_raw();

            let mut host = FakeHost::new(state);
            host.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_a.clone()), ("acct-b", codex_b.clone())],
            )]));
            host.owners.insert(2001, owner_entry(2001, "b2001", uid));
            host.owners.insert(2002, owner_entry(2002, "b2002", uid));

            // Fresh readings pick acct-b (lower weekly usage) with account_pinned=false.
            let now = oms_panel_core::timestamp();
            let write_reading = |home: &Path, used, observed, reset| {
                let dir = home.join("sessions/2026/11/19");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("rollout-fixture.jsonl"),
                    format!("{}\n", json!({
                        "timestamp": observed,
                        "rate_limits": {
                            "primary": {"window_minutes":10080, "used_percent":used, "resets_at":reset},
                            "secondary": {"window_minutes":300, "used_percent":40, "resets_at":now + 1800}
                        }
                    })),
                ).unwrap();
            };
            for (home, used) in [(&codex_a, 70), (&codex_b, 14), (&moved, 50)] {
                write_reading(home, used, now as f64 - 10.25, now + 3600);
            }
            let config = private_dir(&private_dir(&home.join(".config")).join("oh-my-setting"));
            let registry = config.join("provider-accounts.json");
            let write_registry = |backup: &Path| {
                std::fs::write(
                    &registry,
                    serde_json::to_vec(&json!({"schema":1,"providers":{"codex":{"accounts":[
                        {"id":"acct-a","home":codex_a}, {"id":"acct-b","home":backup}
                    ]}}}))
                    .unwrap(),
                )
                .unwrap();
                std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            };
            write_registry(&codex_b);
            host.readings.insert(
                "codex".into(),
                json!({
                    "acct-a": { "weekly_used": 70, "observed_at": now - 15, "resets_at": now + 3600 },
                    "acct-b": { "weekly_used": 10, "observed_at": now - 15, "resets_at": now + 3600 },
                }),
            );
            let spy = SpyPanel::new(repo.clone());
            let mut obs = Observations::default();
            let options = Options::parse(&commands::strings(&["--watch", "--room", ROOM])).unwrap();
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "main-birth", uid));
            let session = commands::Session::new();
            let snapshot = || {
                commands::snapshot_with(&session, &repo, &options, &QuotaReads, &spy.stop).unwrap()
            };
            let weekly_line = |report: &Value| {
                crate::render::frame(report, &Default::default(), 120, 24, &Default::default())
                    .into_iter()
                    .find(|line| line.contains("Weekly left"))
                    .unwrap()
            };
            let assert_no_ambient_quota = |report: &Value| {
                let status = &report["provider_status"]["codex"];
                for key in [
                    "weekly_used",
                    "short_windows",
                    "five_hour_used",
                    "account_label",
                ] {
                    assert!(status.get(key).is_none(), "{key}");
                }
            };
            let mut unbound_main = snapshot().report;
            let ambient_main = unbound_main["provider_status"]["codex"].clone();
            assert_eq!(ambient_main["weekly_used"], 70);
            unbound_main["codex_weekly_account"] = json!("acct-a");
            spy.load()
                .observe_with_host(&mut obs, &mut unbound_main, false, &host);
            assert_eq!(unbound_main["provider_status"]["codex"], ambient_main);
            assert!(unbound_main.get("codex_weekly_account").is_none());
            let unbound_detail = crate::render::weekly_usage_detail(&unbound_main, now as f64);
            assert!(
                unbound_detail.starts_with("Codex · 30% left\n  Source Codex session log"),
                "{unbound_detail}"
            );
            assert!(!unbound_detail.contains("Codex · @"), "{unbound_detail}");
            assert!(
                unbound_detail.contains(
                    "  Registered Codex accounts (Codex session log)\n  \
                     @acct-a · 30% left · Log age 10s · Reset in 1h00m\n  \
                     @acct-b · 86% left · Log age 10s · Reset in 1h00m"
                ),
                "{unbound_detail}"
            );

            spy.main_account_ready.set(true);
            let mut no_binding = snapshot().report;
            spy.load()
                .observe_with_host(&mut obs, &mut no_binding, false, &host);
            assert_no_ambient_quota(&no_binding);
            assert!(no_binding.get("codex_weekly_account").is_none());

            let main = spy.load();
            let main_anchor = BindingAnchor {
                repo: &repo,
                room: ROOM,
                main_attempt: MAIN,
                main_owner: "codex",
                session: &main.session_proof,
                provider: "codex",
                pane_id: &main.main.id,
                pid: spy.main_pid,
                birth: "main-birth",
            };
            let main_account =
                account_binding::resolve_main_selection(&host, "codex", None, "acct-b", "")
                    .unwrap();
            account_binding::save_binding(&host, "main", &main_anchor, &main_account).unwrap();
            let mut main_report = snapshot().report;
            assert_eq!(main_report["provider_status"]["codex"]["weekly_used"], 70);
            let mut no_receipt = main_report.clone();
            spy.load()
                .observe_with_host(&mut obs, &mut main_report, false, &host);
            assert_eq!(main_report["provider_status"]["codex"]["weekly_used"], 14);
            assert_eq!(main_report["codex_weekly_account"], "acct-b");
            let main_detail = crate::render::weekly_usage_detail(&main_report, now as f64);
            assert!(
                main_detail.starts_with("Codex · @acct-b · 86% left\n"),
                "{main_detail}"
            );
            assert!(
                main_detail.contains(
                    "  Registered Codex accounts (Codex session log)\n  \
                     @acct-a · 30% left · Log age 10s · Reset in 1h00m\n  \
                     @acct-b · 86% left · Log age 10s · Reset in 1h00m"
                ),
                "{main_detail}"
            );
            assert!(weekly_line(&main_report).contains("Weekly left · Codex 86%"));
            assert!(main_report["provider_accounts"].get("codex").is_none());
            let status = &main_report["provider_status"]["codex"];
            assert!(status.get("account_label").is_none() && status.get("short_windows").is_none());
            let public = main_report.to_string();
            for home in [&codex_a, &codex_b] {
                assert!(!public.contains(home.to_str().unwrap()));
                assert!(!public.contains(&account_binding::path_digest(home)));
            }

            no_receipt["codex_weekly_account"] = json!("acct-b");
            spy.load()
                .observe_with_host(&mut obs, &mut no_receipt, false, &host);
            assert_no_ambient_quota(&no_receipt);
            assert!(no_receipt.get("codex_weekly_account").is_none());
            assert!(
                !crate::render::weekly_usage_detail(&no_receipt, now as f64).contains("Codex · @")
            );
            for case in [
                "other birth",
                "pi harness",
                "terminal membership",
                "capacity stale",
                "stopped",
            ] {
                let mut report = snapshot().report;
                match case {
                    "other birth" => {
                        host.owners
                            .insert(spy.main_pid, owner_entry(spy.main_pid, "other-birth", uid));
                    }
                    "pi harness" => {
                        report["room_attempts"][MAIN]["refs"]["panel_harness"] = "pi".into()
                    }
                    "capacity stale" => report["capacity_stale"] = true.into(),
                    "stopped" => spy.stop.store(true, Ordering::Release),
                    _ => report["room"]["participants"][0]["state"] = "done".into(),
                }
                spy.load()
                    .observe_with_host(&mut obs, &mut report, false, &host);
                spy.stop.store(false, Ordering::Release);
                assert_no_ambient_quota(&report);
                assert!(report.get("codex_weekly_account").is_none(), "{case}");
                assert!(
                    !crate::render::weekly_usage_detail(&report, now as f64).contains("Codex · @"),
                    "{case}"
                );
                host.owners
                    .insert(spy.main_pid, owner_entry(spy.main_pid, "main-birth", uid));
            }

            let lead = spy
                .load()
                .execute_with_host("codex", Some("acct-a"), None, &host)
                .unwrap();
            assert_eq!(lead["account"], "acct-a");
            spy.leads.borrow_mut()[0].active = true;
            let mut lead_report = snapshot().report;
            spy.load()
                .observe_with_host(&mut obs, &mut lead_report, false, &host);
            assert!(weekly_line(&lead_report).contains("Weekly left · Codex 30%"));
            assert_eq!(
                lead_report["provider_accounts"]["codex"]["account"],
                "acct-a"
            );
            assert_eq!(lead_report["codex_weekly_account"], "acct-a");
            let lead_detail = crate::render::weekly_usage_detail(&lead_report, now as f64);
            assert!(
                lead_detail.starts_with("Codex · @acct-a · 30% left\n"),
                "{lead_detail}"
            );
            for case in ["capacity stale", "stopped", "foreign main"] {
                let mut report = snapshot().report;
                match case {
                    "capacity stale" => report["capacity_stale"] = true.into(),
                    "stopped" => spy.stop.store(true, Ordering::Release),
                    _ => {
                        host.owners.insert(
                            spy.main_pid,
                            owner_entry(spy.main_pid, "main-birth", uid + 1),
                        );
                    }
                }
                spy.load()
                    .observe_with_host(&mut obs, &mut report, false, &host);
                spy.stop.store(false, Ordering::Release);
                host.owners
                    .insert(spy.main_pid, owner_entry(spy.main_pid, "main-birth", uid));
                assert_no_ambient_quota(&report);
                assert!(report.get("codex_weekly_account").is_none(), "{case}");
                assert!(
                    !crate::render::weekly_usage_detail(&report, now as f64).contains("Codex · @"),
                    "{case}"
                );
            }
            spy.leads.borrow_mut().clear();

            let ranked = spy
                .load()
                .execute_with_host("codex", None, None, &host)
                .unwrap();
            assert_eq!(ranked["status"], "started");
            assert_eq!(ranked["account"], "acct-b");
            assert_eq!(ranked["account_pinned"], false);
            assert_eq!(ranked["account_verification"], "verified");

            // Unpinned Claude with no fresh ranking resolves as UnpinnedAmbient.
            spy.spawn_pid.set(2002);
            let ambient = spy
                .load()
                .execute_with_host("claude", None, None, &host)
                .unwrap();
            assert_eq!(ambient["status"], "started");
            assert_eq!(ambient["account"], Value::Null);
            assert_eq!(ambient["account_pinned"], false);
            assert_eq!(ambient["account_verification"], "unpinned_ambient");

            let mut report = snapshot().report;
            let mut unbound = report.clone();
            let mut replay = report.clone();
            assert_eq!(report["provider_status"]["codex"]["weekly_used"], 70);
            assert_eq!(
                report["provider_status"]["codex"]["accounts"]["acct-b"]["weekly_used"],
                14
            );
            let published = spy.load().observation_generation_with_host(&host).unwrap();
            assert!(published.main_account.is_some() && published.lead_accounts[0].is_some());
            let mut remapped = host.clone();
            remapped.accounts = Some(make_accounts(vec![(
                "codex",
                vec![("acct-a", codex_a.clone()), ("acct-b", moved.clone())],
            )]));
            write_registry(&moved);
            assert!(
                spy.load()
                    .observation_generation_with_host(&remapped)
                    .is_err()
            );
            spy.main_account_ready.set(false);
            assert!(
                spy.load()
                    .observation_generation_with_host(&remapped)
                    .is_err()
            );
            spy.main_account_ready.set(true);
            spy.load()
                .observe_with_host(&mut obs, &mut report, false, &host);
            assert_eq!(
                report["provider_accounts"]["codex"],
                json!({"account": "acct-b", "pinned": false, "verification": "verified"})
            );
            assert_eq!(
                report["provider_accounts"]["claude"],
                json!({"account": null, "pinned": false, "verification": "unpinned_ambient"})
            );
            assert_eq!(report["codex_weekly_account"], "acct-b");
            let quota = &report["provider_status"]["codex"];
            assert_eq!(quota["weekly_used"], 14);
            assert_eq!(quota["weekly_resets_at"], now as f64 + 3600.0);
            assert_eq!(quota["observed_at"], now as f64 - 10.25);
            assert!(quota.get("account_label").is_none());
            assert!(quota.get("five_hour_used").is_none());
            let output =
                crate::render::frame(&report, &Default::default(), 120, 24, &Default::default())
                    .join("\n");
            assert!(output.contains("Weekly left · Codex 86%"), "{output}");

            spy.load()
                .observe_with_host(&mut obs, &mut replay, false, &host);
            assert!(
                replay["provider_status"]["codex"]
                    .get("weekly_used")
                    .is_none()
            );
            assert!(replay.get("codex_weekly_account").is_none());
            let mut new_source = snapshot().report;
            assert_eq!(
                new_source["provider_status"]["codex"]["accounts"]["acct-b"]["weekly_used"],
                50
            );
            let sources =
                commands::take_codex_quota_receipt(&repo, ROOM, MAIN, &new_source).unwrap();
            assert!(!sources.account_matches("acct-b", &account_binding::path_digest(&codex_b)));
            assert!(sources.account_matches("acct-b", &account_binding::path_digest(&moved)));
            let public = new_source.to_string();
            for home in [&codex_a, &codex_b, &moved] {
                assert!(!public.contains(home.to_str().unwrap()));
                assert!(!public.contains(&account_binding::path_digest(home)));
            }
            assert!(!public.contains("home_digest") && !public.contains("quota_receipt"));
            new_source = snapshot().report;
            spy.load()
                .observe_with_host(&mut obs, &mut new_source, false, &host);
            assert!(
                new_source["provider_status"]["codex"]
                    .get("weekly_used")
                    .is_none()
            );
            assert!(new_source.get("codex_weekly_account").is_none());
            assert!(
                !crate::render::weekly_usage_detail(&new_source, now as f64).contains("Codex · @")
            );

            // PID reuse invalidates the stored binding and leaves the snapshot untouched.
            host.owners
                .insert(2001, owner_entry(2001, "other-birth", uid));
            spy.leads
                .borrow_mut()
                .retain(|lead| lead.provider == "codex");
            let ambient_before = unbound["provider_status"]["codex"].clone();
            unbound["codex_weekly_account"] = json!("acct-b");
            spy.load()
                .observe_with_host(&mut obs, &mut unbound, false, &host);
            assert_eq!(
                unbound["provider_accounts"]["codex"]["verification"],
                "unverified"
            );
            assert_eq!(unbound["provider_status"]["codex"], ambient_before);
            assert!(unbound.get("codex_weekly_account").is_none());
            let reused_detail = crate::render::weekly_usage_detail(&unbound, now as f64);
            assert!(
                reused_detail.starts_with("Codex · 30% left\n"),
                "{reused_detail}"
            );
            assert!(!reused_detail.contains("Codex · @"), "{reused_detail}");

            spy.leads.borrow_mut().clear();
            let mut main_remapped = snapshot().report;
            spy.load()
                .observe_with_host(&mut obs, &mut main_remapped, false, &host);
            assert_eq!(
                main_remapped["provider_status"]["codex"]["accounts"]["acct-b"]["weekly_used"],
                50
            );
            assert_no_ambient_quota(&main_remapped);
            assert!(main_remapped.get("codex_weekly_account").is_none());
            assert!(
                !crate::render::weekly_usage_detail(&main_remapped, now as f64)
                    .contains("Codex · @")
            );

            write_registry(&codex_b);
            for (reset, expected) in [(now + 3600, "Codex stale"), (now - 1, "Codex —")] {
                write_reading(&codex_b, 14, now as f64 - 1200.0, reset);
                let mut report = commands::snapshot_with(
                    &commands::Session::new(),
                    &repo,
                    &options,
                    &QuotaReads,
                    &spy.stop,
                )
                .unwrap()
                .report;
                spy.load()
                    .observe_with_host(&mut obs, &mut report, false, &host);
                let weekly = weekly_line(&report);
                assert!(weekly.contains(expected), "{weekly}");
                assert!(!weekly.contains("86%") && !weekly.contains("30%"));
            }
        }

        #[test]
        fn bound_codex_quota_uses_only_proved_short_windows() {
            let now = 10_000.75;
            let original = json!({"provider_status":{"codex":{
                "source":"session log", "weekly_used":70, "weekly_resets_at":now+1500.0,
                "observed_at":now-1.0, "weekly_native_observed_at":now-3.0,
                "five_hour_used":90, "five_hour_resets_at":now+1200.0,
                "short_windows":[
                    {"minutes":60, "used":20, "resets_at":now+1800.0, "observed_at":now-2.0},
                    {"minutes":300, "used":90, "resets_at":now+1200.0, "observed_at":null},
                    {"minutes":180, "used":50, "resets_at":now-1.0, "observed_at":now-3.0},
                    {"minutes":240, "used":60, "resets_at":now+1800.0, "observed_at":now-601.0}
                ],
                "accounts":{"backup":{"source":"session log", "weekly_used":14,
                    "observed_at":now-0.25, "resets_at":now+3600.0}}
            }}});
            for ambient in [None, Some("primary"), Some("backup")] {
                let mut report = original.clone();
                if let Some(ambient) = ambient {
                    report["provider_status"]["codex"]["account_label"] = json!(ambient);
                }
                project_bound_codex_quota(&mut report, "backup", now, true, true);
                assert_eq!(report["codex_weekly_account"], "backup");
                let mut detail_report = report.clone();
                detail_report["collection"] = json!({"ok": true});
                let detail = crate::render::weekly_usage_detail(&detail_report, now);
                assert!(
                    detail.starts_with("Codex · @backup · 86% left\n"),
                    "{detail}"
                );
                let quota = &report["provider_status"]["codex"];
                assert_eq!(quota["weekly_used"], 14);
                assert_eq!(quota["observed_at"], now - 0.25);
                assert_eq!(
                    quota["account_label"].as_str(),
                    ambient.filter(|label| *label == "backup")
                );
                assert!(quota.get("weekly_native_observed_at").is_none());
                assert!(quota.get("five_hour_used").is_none());
                assert!(quota.get("five_hour_resets_at").is_none());
                if ambient == Some("backup") {
                    assert_eq!(
                        quota["short_windows"],
                        json!([original["provider_status"]["codex"]["short_windows"][0].clone()])
                    );
                } else {
                    assert!(quota.get("short_windows").is_none());
                }
            }
            let mut modern = original.clone();
            let status = &mut modern["provider_status"]["codex"];
            status["account_label"] = json!("backup");
            status["short_windows"][1]["used"] = json!(35);
            status["short_windows"][1]["observed_at"] = json!(now - 2.0);
            project_bound_codex_quota(&mut modern, "backup", now, true, true);
            assert_eq!(modern["provider_status"]["codex"]["five_hour_used"], 35);
            assert_eq!(modern["codex_weekly_account"], "backup");

            let mut legacy = original;
            let status = legacy["provider_status"]["codex"].as_object_mut().unwrap();
            status.remove("short_windows");
            status.insert("account_label".into(), json!("backup"));
            let mut unproved = legacy.clone();
            unproved["codex_weekly_account"] = json!("backup");
            project_bound_codex_quota(&mut unproved, "backup", now, false, false);
            assert!(unproved.get("codex_weekly_account").is_none());
            let quota = &unproved["provider_status"]["codex"];
            for key in [
                "account_label",
                "weekly_used",
                "five_hour_used",
                "five_hour_resets_at",
            ] {
                assert!(quota.get(key).is_none(), "unproved {key}");
            }
            project_bound_codex_quota(&mut legacy, "backup", now, true, true);
            assert_eq!(legacy["provider_status"]["codex"]["five_hour_used"], 90);
            assert_eq!(legacy["codex_weekly_account"], "backup");
        }

        #[test]
        fn bound_codex_quota_preserves_stale_and_elapsed_evidence_but_refuses_malformed() {
            let now = 10_000.75;
            let original = json!({"provider_status":{"codex":{
                "source":"session log", "account_label":"primary",
                "weekly_used":70, "weekly_resets_at":now+1800.0, "observed_at":now-3.0,
                "accounts":{"backup":{"source":"session log", "weekly_used":14,
                    "observed_at":now-601.0, "resets_at":now+0.25}}
            }}});
            let mut report = original.clone();
            project_bound_codex_quota(&mut report, "backup", now, true, true);
            assert_eq!(report["codex_weekly_account"], "backup");
            let mut stale_detail_report = report.clone();
            stale_detail_report["collection"] = json!({"ok": true});
            let stale_detail = crate::render::weekly_usage_detail(&stale_detail_report, now);
            assert!(
                stale_detail.contains("Codex · @backup · stale (10m)")
                    && stale_detail.contains("  @backup · stale · Log age 10m · Reset unreported"),
                "{stale_detail}"
            );
            let usage = crate::usage::snapshot(&report, now);
            assert_eq!(usage["providers"]["codex"]["reading"]["stale"], true);
            assert_eq!(
                usage["providers"]["codex"]["reading"]["source"],
                "Codex session log"
            );
            assert_eq!(
                usage["providers"]["codex"]["windows"][0]["resets_at"],
                now + 0.25
            );

            let mut elapsed = original.clone();
            project_bound_codex_quota(&mut elapsed, "backup", now + 0.5, true, true);
            assert_eq!(elapsed["codex_weekly_account"], "backup");
            assert_eq!(elapsed["provider_status"]["codex"]["weekly_used"], 14);
            assert_eq!(
                elapsed["provider_status"]["codex"]["weekly_resets_at"],
                now + 0.25
            );
            let mut elapsed_detail_report = elapsed.clone();
            elapsed_detail_report["collection"] = json!({"ok": true});
            let elapsed_detail =
                crate::render::weekly_usage_detail(&elapsed_detail_report, now + 0.5);
            assert!(
                elapsed_detail.contains("Codex · @backup · window expired")
                    && elapsed_detail
                        .contains("  @backup · window expired · Log age 10m · Reset elapsed"),
                "{elapsed_detail}"
            );
            let elapsed_usage = crate::usage::snapshot(&elapsed, now + 0.5);
            assert!(elapsed_usage["providers"]["codex"]["windows"][0]["resets_at"].is_null());

            for (field, invalid) in [
                ("source", json!("API")),
                ("weekly_used", json!(120)),
                ("observed_at", json!(now + 0.1)),
                ("observed_at", Value::Null),
                ("resets_at", json!(now - 602.0)),
                ("resets_at", json!("malformed")),
            ] {
                let mut missing = original.clone();
                missing["codex_weekly_account"] = json!("backup");
                missing["provider_status"]["codex"]["accounts"]["backup"][field] = invalid;
                project_bound_codex_quota(&mut missing, "backup", now, true, true);
                assert!(missing.get("codex_weekly_account").is_none(), "{field}");
                let quota = &missing["provider_status"]["codex"];
                assert_eq!(quota["source"], "unavailable", "{field}");
                for key in ["weekly_used", "observed_at", "weekly_resets_at"] {
                    assert!(quota.get(key).is_none(), "{field} left {key}");
                }
                missing["collection"] = json!({"ok": true});
                let detail = crate::render::weekly_usage_detail(&missing, now);
                assert!(!detail.contains("Codex · @"), "{field}: {detail}");
            }
            let mut unknown = original;
            unknown["codex_weekly_account"] = json!("other");
            project_bound_codex_quota(&mut unknown, "other", now, false, false);
            assert!(unknown.get("codex_weekly_account").is_none());
            assert!(
                unknown["provider_status"]["codex"]
                    .get("weekly_used")
                    .is_none()
            );
        }

        #[test]
        fn codex_chat_uses_one_bound_root_and_revalidates_the_read() {
            use crate::native_children::{TASK_PREFIX, seed_codex_chat_rollouts};
            use crate::orchestrator::codex_chat_fixture::{NEW_TURN, TURN};

            for requested in [Some("alternate"), None] {
                let tmp = TempDir::new("orch-bound-chat");
                let repo = private_dir(&tmp.0.join("repo"));
                let state = private_dir(&tmp.0.join("state"));
                let alternate = private_dir(&tmp.0.join("alternate-home"));
                let ambient = private_dir(&tmp.0.join("ambient-home"));
                let registry = |home: &Path| {
                    make_accounts(vec![(
                        "codex",
                        vec![("alternate", home.to_owned()), ("ambient", ambient.clone())],
                    )])
                };
                let mut host = FakeHost::new(state.clone());
                host.accounts = Some(registry(&alternate));
                host.active_homes.insert("codex".into(), ambient.clone());
                let now = oms_panel_core::timestamp();
                host.readings.insert(
                    "codex".into(),
                    json!({
                        "alternate":{"weekly_used":14,"observed_at":now-5,"resets_at":now+3600},
                        "ambient":{"weekly_used":70,"observed_at":now-5,"resets_at":now+3600}
                    }),
                );
                let spy = SpyPanel::new(repo.clone());
                host.owners.insert(
                    spy.main_pid,
                    owner_entry(spy.main_pid, "main-birth", host.uid),
                );
                host.owners
                    .insert(2001, owner_entry(2001, "lead-birth", host.uid));
                let participant = seed_codex_chat_rollouts(
                    &alternate,
                    &repo,
                    TURN,
                    "Bound Worker",
                    "Proved alternate reply",
                );
                seed_codex_chat_rollouts(
                    &ambient,
                    &repo,
                    TURN,
                    "Decoy Worker",
                    "Ambient decoy reply",
                );
                let launch = spy
                    .load()
                    .execute_with_host("codex", requested, None, &host)
                    .unwrap();
                assert_eq!(launch["account"], "alternate");
                assert_eq!(launch["account_pinned"], requested.is_some());
                let lead = spy.leads.borrow()[0].pane_id.clone();
                let launch_calls = spy.mutating_calls();
                let marked = launch_calls
                    .iter()
                    .position(|args| args.iter().any(|arg| arg == "@oms_codex_chat_bound"))
                    .unwrap();
                let admitted = launch_calls
                    .iter()
                    .position(|args| args.iter().any(|arg| arg == "@oms_provider_session"))
                    .unwrap();
                assert!(marked < admitted);
                spy.allow_board_queries.set(true);
                let prompt =
                    |turn| format!("› {TASK_PREFIX} Turn {turn}. You own this provider's work.");
                spy.recent_screens
                    .borrow_mut()
                    .insert(lead.clone(), prompt(TURN));
                let observe_into = |host: &FakeHost, observations: &mut Observations| {
                    let mut report = accepted_codex_scope();
                    Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop)
                        .unwrap()
                        .observe_with_host(observations, &mut report, true, host);
                    report
                };
                let observe = |host: &FakeHost| {
                    let mut observations = Observations::default();
                    let report = observe_into(host, &mut observations);
                    (observations, report)
                };
                let attempts = AttemptCache::default();
                let open = |host: &FakeHost, binding: &ChatBinding| {
                    let mut board =
                        Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
                    board.chat_host = Some(host);
                    board.open_chat(binding, None, &attempts)
                };

                let (mut rows, report) = observe(&host);
                if std::fs::metadata(&alternate)
                    .unwrap()
                    .created()
                    .ok()
                    .is_none_or(|created| created <= std::time::UNIX_EPOCH)
                {
                    assert_eq!(report["native_children"], json!([]));
                    continue;
                }
                let binding = rows
                    .chat_binding(&participant)
                    .expect("painted bound child")
                    .clone();
                assert_eq!(report["native_children"][0]["title"], "Bound Worker");
                let public = report.to_string();
                for home in [&alternate, &ambient] {
                    assert!(!public.contains(home.to_str().unwrap()));
                    assert!(!public.contains(&account_binding::path_digest(home)));
                }
                assert!(!public.contains("Proved alternate reply"));
                assert_eq!(
                    open(&host, &binding).as_deref(),
                    Some("Proved alternate reply")
                );
                if requested.is_none() {
                    continue;
                }
                let assert_withheld = |host: &FakeHost| {
                    let (rows, report) = observe(host);
                    assert!(rows.chat_binding(&participant).is_none());
                    assert_eq!(report["native_children"], json!([]));
                    assert!(open(host, &binding).is_none());
                };

                *spy.next_recent.borrow_mut() = Some(prompt(NEW_TURN));
                assert_eq!(observe(&host).1["native_children"], json!([]));
                spy.recent_screens
                    .borrow_mut()
                    .insert(lead.clone(), prompt(TURN));
                let mut board = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
                board.chat_host = Some(&host);
                assert!(
                    board
                        .open_chat_with(&binding, None, &attempts, |_, _, _| {
                            spy.stop.store(true, Ordering::Release);
                            Some("discarded on cancellation".into())
                        })
                        .is_none()
                );
                assert_eq!(observe(&host).1["native_children"], json!([]));
                spy.stop.store(false, Ordering::Release);
                let old_dir = tmp.0.join("saved-home");
                assert!(
                    board
                        .open_chat_with(&binding, None, &attempts, |_, _, _| {
                            std::fs::rename(&alternate, &old_dir).unwrap();
                            private_dir(&alternate);
                            Some("discarded after the root changed".into())
                        })
                        .is_none()
                );
                std::fs::remove_dir(&alternate).unwrap();
                std::fs::rename(&old_dir, &alternate).unwrap();
                drop(board);

                host.accounts = Some(registry(&private_dir(&tmp.0.join("remapped-home"))));
                assert_withheld(&host);
                host.accounts = None;
                assert_withheld(&host);
                host.accounts = Some(registry(&alternate));
                std::fs::rename(&alternate, &old_dir).unwrap();
                private_dir(&alternate);
                seed_codex_chat_rollouts(&alternate, &repo, TURN, "Replacement", "Wrong directory");
                assert_withheld(&host);
                std::fs::remove_dir_all(&alternate).unwrap();
                std::fs::rename(&old_dir, &alternate).unwrap();

                let bindings = state
                    .join("oh-my-setting/panel-control")
                    .join(account_binding::path_digest(&repo))
                    .join("account-bindings");
                let receipt = std::fs::read_dir(&bindings)
                    .unwrap()
                    .flatten()
                    .find(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
                    .unwrap()
                    .path();
                let original = std::fs::read(&receipt).unwrap();
                std::fs::remove_file(&receipt).unwrap();
                assert_withheld(&host);
                let mut marked_only = Observations::default();
                spy.recent_screens.borrow_mut().remove(&lead);
                assert_eq!(
                    observe_into(&host, &mut marked_only)["native_children"],
                    json!([])
                );
                spy.recent_screens
                    .borrow_mut()
                    .insert(lead.clone(), prompt(TURN));
                std::fs::write(&receipt, b"corrupt receipt").unwrap();
                std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
                assert_withheld(&host);
                std::fs::write(&receipt, &original).unwrap();
                spy.codex_chat_marks
                    .borrow_mut()
                    .insert(lead.clone(), "invalid".into());
                assert_withheld(&host);
                spy.codex_chat_marks.borrow_mut().remove(&lead);
                assert_withheld(&host); // A new-format receipt alone cannot downgrade.

                let mut legacy: Value = serde_json::from_slice(&original).unwrap();
                legacy
                    .as_object_mut()
                    .unwrap()
                    .remove("codex_home_identity");
                std::fs::write(&receipt, serde_json::to_vec(&legacy).unwrap()).unwrap();
                assert_withheld(&host); // Wrong active home for the legacy receipt.
                host.active_homes.insert("codex".into(), alternate.clone());
                assert_eq!(observe_into(&host, &mut rows)["native_children"], json!([]));
                let (legacy_rows, legacy_report) = observe(&host);
                assert_eq!(legacy_report["native_children"][0]["title"], "Bound Worker");
                assert_eq!(
                    open(&host, legacy_rows.chat_binding(&participant).unwrap()).as_deref(),
                    Some("Proved alternate reply")
                );

                std::fs::remove_file(&receipt).unwrap();
                host.accounts = None;
                host.active_homes.insert("codex".into(), ambient.clone());
                {
                    let mut leads = spy.leads.borrow_mut();
                    leads[0].active = true;
                    leads[0].window = "@1".into();
                }
                for held_board in [&mut rows, &mut marked_only] {
                    let held = observe_into(&host, held_board);
                    assert_eq!(held["native_children"], json!([]));
                    assert!(held_board.chat_binding(&participant).is_none());
                }
                spy.leads.borrow_mut()[0].window = "@4".into();
                assert!(open(&host, &binding).is_none());

                // Fresh pre-binding observations and new unbound panes use their local ambient root.
                assert_eq!(
                    observe(&host).1["native_children"][0]["title"],
                    "Decoy Worker"
                );
                spy.leads.borrow_mut().clear();
                let unbound = spy
                    .load()
                    .execute_with_host("codex", None, None, &host)
                    .unwrap();
                assert_eq!(unbound["account_verification"], "unpinned_ambient");
                let lead = spy.leads.borrow()[0].pane_id.clone();
                spy.recent_screens.borrow_mut().insert(lead, prompt(TURN));
                let ambient_report = observe_into(&host, &mut rows);
                assert_eq!(
                    ambient_report["native_children"][0]["title"],
                    "Decoy Worker"
                );
                assert_eq!(
                    open(&host, rows.chat_binding(&participant).unwrap()).as_deref(),
                    Some("Ambient decoy reply")
                );
            }
        }

        #[test]
        fn agy_sighting_requires_unique_painted_row_matching_turn_and_pane() {
            let turn = super::super::codex_chat_fixture::TURN;
            let child_a = "22222222-2222-2222-2222-bbbbbbbbbbbb";
            let pid_a = crate::native_children::participant_id("antigravity", child_a);
            let rows = vec![json!({
                "participant": pid_a,
                "native_child": true,
                "provider": "antigravity",
                "native_answer": "parent summary"
            })];

            // Unique painted row with live Turn and pid/birth binds ONLY that painted row.
            let bound = agy_sighting(&rows, turn, 42, "boot:1");
            assert_eq!(bound.len(), 1);
            assert_eq!(bound[0].0, pid_a);
            assert!(matches!(bound[0].1, NativeLocator::Antigravity(_)));

            // Duplicate painted rows for the same participant refuse binding.
            let dup_rows = vec![rows[0].clone(), rows[0].clone()];
            assert!(agy_sighting(&dup_rows, turn, 42, "boot:1").is_empty());

            // Empty Turn or invalid pid/birth drops the sighting.
            assert!(agy_sighting(&rows, "", 42, "boot:1").is_empty());
            assert!(agy_sighting(&rows, turn, 0, "boot:1").is_empty());
            assert!(agy_sighting(&rows, turn, 42, "").is_empty());
        }

        #[test]
        fn agy_usage_attaches_one_fenced_scan_to_unique_painted_rows_only() {
            use std::collections::BTreeSet;
            let turn = super::super::codex_chat_fixture::TURN;
            let ids = [
                "11111111-1111-1111-1111-aaaaaaaaaaaa",
                "22222222-2222-2222-2222-bbbbbbbbbbbb",
                "33333333-3333-3333-3333-cccccccccccc",
                "44444444-4444-4444-4444-dddddddddddd",
            ];
            let [a, b, c, d] =
                ids.map(|id| crate::native_children::participant_id("antigravity", id));
            let row = |participant: &str| json!({"participant": participant, "native_child": true, "provider": "antigravity"});
            let usage = |n: u64| {
                json!({"model": "gemini", "created_at": "2026-11-22T00:00:00Z",
                       "input_tokens": n, "cached_input_tokens": 0, "output_tokens": 1})
            };
            let all: BTreeMap<String, Value> = [&a, &b, &c, &d]
                .into_iter()
                .zip(1..)
                .map(|(p, n)| (p.clone(), usage(n)))
                .collect();
            let field = |n: u64| json!({"scope": "latest_child_response", "response": usage(n)});
            // One sample: the rows after it, the set offered to its scans, and proofs run.
            let sample = |mut rows: Vec<Value>, shown: &str, scan_turn: &str, proof: bool| {
                let (offered, proofs) = (RefCell::new(Vec::new()), Cell::new(0));
                agy_usage_sighting(
                    &mut rows,
                    shown,
                    |painted: &BTreeSet<String>| {
                        offered.borrow_mut().push(painted.clone());
                        Some((scan_turn.to_owned(), all.clone()))
                    },
                    || {
                        proofs.set(proofs.get() + 1);
                        proof
                    },
                );
                (rows, offered.into_inner(), proofs.get())
            };
            let set = |ps: &[&String]| ps.iter().map(|p| (*p).clone()).collect::<BTreeSet<_>>();
            let unrelated = json!({"participant": "native-claude-x", "state": "working"});

            // Exact participant only; the helper's object goes on as `response`
            // verbatim, with no total computed from its counters.
            let rows = vec![row(&b), unrelated.clone()];
            let (got, offered, proofs) = sample(rows.clone(), turn, turn, true);
            assert_eq!((offered, proofs), (vec![set(&[&b])], 1));
            assert_eq!(got[0]["native_response_usage"], field(2));
            assert_eq!(got[0]["observed_model"], "gemini");
            assert!(
                got[0]["native_response_usage"]["response"]
                    .get("total_tokens")
                    .is_none()
            );
            assert_eq!(got[1], unrelated);
            // Repeating the same sample gives the same rows with one scan each.
            assert_eq!(sample(rows, turn, turn, true).0, got);

            // A participant on two rows (painted twice, or also named by another row)
            // is never offered or attached, even if the scan returns it.
            let rows = vec![
                row(&a),
                row(&a),
                row(&b),
                row(&c),
                json!({"participant": c}),
            ];
            let (got, offered, _) = sample(rows.clone(), turn, turn, true);
            assert_eq!(offered, vec![set(&[&b])]);
            assert!(got[0] == rows[0] && got[1] == rows[1] && got[3] == rows[3]);
            assert_eq!(got[2]["native_response_usage"], field(2));
            assert_eq!(got[2]["observed_model"], "gemini");
            assert!(
                got.iter()
                    .enumerate()
                    .all(|(i, r)| i == 2 || r.get("observed_model").is_none())
            );

            // More than three eligible: only the first three in participant order.
            let mut order = [&a, &b, &c, &d];
            order.sort();
            let rows: Vec<Value> = order.iter().rev().map(|p| row(p)).collect();
            let (got, offered, _) = sample(rows.clone(), turn, turn, true);
            assert_eq!(offered, vec![set(&order[..3])]);
            assert_eq!(got[0], rows[0]);
            assert!(
                got[1..]
                    .iter()
                    .all(|r| r.get("native_response_usage").is_some())
            );

            // A scan of another Turn, a failed after-fence, or no scan attaches nothing.
            let rows = vec![row(&a)];
            assert_eq!(
                sample(rows.clone(), turn, "0000000000000000dead", true),
                (rows.clone(), vec![set(&[&a])], 0)
            );
            assert_eq!(
                sample(rows.clone(), turn, turn, false),
                (rows.clone(), vec![set(&[&a])], 1)
            );
            let mut none = rows.clone();
            agy_usage_sighting(&mut none, turn, |_| None, || unreachable!());
            assert_eq!(none, rows);
            assert!(none[0].get("observed_model").is_none());

            // A model the shared validator rejects is omitted; the usage is unchanged.
            for bad in ["<synthetic>", "\u{1b}[31mgemini", " gemini"] {
                let mut invalid = usage(2);
                invalid["model"] = json!(bad);
                let scan = BTreeMap::from([(b.clone(), invalid.clone())]);
                let mut rows = vec![row(&b)];
                agy_usage_sighting(
                    &mut rows,
                    turn,
                    |_| Some((turn.to_owned(), scan.clone())),
                    || true,
                );
                assert!(rows[0].get("observed_model").is_none(), "{bad:?}");
                assert_eq!(
                    rows[0]["native_response_usage"],
                    json!({"scope": "latest_child_response", "response": invalid})
                );
            }

            // No eligible row or no shown Turn: neither the scan nor the fence runs.
            for (rows, shown) in [
                (vec![row(&a), row(&a), unrelated.clone()], turn),
                (vec![row(&a), json!({"participant": a})], turn),
                (vec![], turn),
                (vec![row(&a)], ""),
            ] {
                assert_eq!(sample(rows.clone(), shown, turn, true), (rows, vec![], 0));
            }
        }

        #[test]
        fn agy_chat_pane_accepts_open_unverified_binding_and_rejects_unpinned_ambient() {
            let tmp = TempDir::new("orch-agy-binding");
            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let mut host = FakeHost::new(state.clone());
            let spy = SpyPanel::new(repo.clone());
            let uid = host.uid;
            host.owners
                .insert(spy.main_pid, owner_entry(spy.main_pid, "main-birth", uid));
            host.owners
                .insert(2001, owner_entry(2001, "lead-birth", uid));

            let launched = spy
                .load()
                .execute_with_host("antigravity", None, None, &host)
                .unwrap();
            assert_eq!(launched["account_verification"], "unverified");
            assert!(spy.load().chat_pane_with_host("antigravity", &host).is_ok());

            // Overwrite saved binding verification to UnpinnedAmbient: must fail closed.
            let bind_dir = state
                .join("oh-my-setting/panel-control")
                .join(account_binding::path_digest(&repo))
                .join("account-bindings");
            let receipt = std::fs::read_dir(&bind_dir)
                .unwrap()
                .flatten()
                .find(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
                .unwrap()
                .path();
            let mut val: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
            val["verification"] = json!("unpinned_ambient");
            std::fs::write(&receipt, serde_json::to_vec(&val).unwrap()).unwrap();
            std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(
                spy.load()
                    .chat_pane_with_host("antigravity", &host)
                    .is_err()
            );

            // Corrupt / missing receipt metadata with no account is absent; removing yields None (accepted).
            std::fs::remove_file(&receipt).unwrap();
            assert!(spy.load().chat_pane_with_host("antigravity", &host).is_ok());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn agy_observe_and_open_chat_e2e_with_real_private_holder() {
            use crate::native_children::TASK_PREFIX;
            use std::io::{BufRead, BufReader, Write};
            use std::process::{Command, Stdio};

            const ENV_CHILD: &str = "OMS_TEST_AGY_ORCH_E2E";
            const TEST_NAME: &str = "orchestrator::tests::account_tests::agy_observe_and_open_chat_e2e_with_real_private_holder";
            let tmp = TempDir::new("orch-agy-e2e");
            let home = if let Some(h) = std::env::var_os(ENV_CHILD) {
                PathBuf::from(h)
            } else {
                let home = private_dir(&tmp.0.join("home"));
                let output = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST_NAME, "--nocapture"])
                    .env_clear()
                    .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                    .env("HOME", &home)
                    .env("TMPDIR", &tmp.0)
                    .env(ENV_CHILD, &home)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success() && stdout.contains("1 passed"),
                    "stdout:\n{stdout}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            };

            let repo = private_dir(&tmp.0.join("repo"));
            let state = private_dir(&tmp.0.join("state"));
            let gemini = private_dir(&home.join(".gemini"));
            let app_root = private_dir(&gemini.join("antigravity-cli"));
            let presence = private_dir(&app_root.join("presence"));
            let brain = private_dir(&app_root.join("brain"));

            // Init git repo so .git exists under 0o700 and can later be chmodded 0o775.
            for args in [
                vec!["init", "-q"],
                vec!["commit", "-q", "--allow-empty", "-m", "init"],
            ] {
                let status = Command::new("git")
                    .current_dir(&repo)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                    .args(["-c", "commit.gpgsign=false"])
                    .args(args)
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            fn chmod_tree(path: &Path) {
                let meta = std::fs::symlink_metadata(path).unwrap();
                if meta.file_type().is_symlink() {
                    return;
                }
                if meta.is_dir() {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
                    for entry in std::fs::read_dir(path).unwrap() {
                        chmod_tree(&entry.unwrap().path());
                    }
                } else {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            let lead_id = "aaaaaaaa-1111-2222-3333-444444444444";
            let child_id = "22222222-2222-2222-2222-bbbbbbbbbbbb";
            let legacy_child = "33333333-3333-3333-3333-cccccccccccc";
            let wt_parent = tmp.0.join("wt-parent");
            std::fs::create_dir(&wt_parent).unwrap();
            let wt = wt_parent.join("repo-wt");
            let wt_status = Command::new("git")
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(["-c", "commit.gpgsign=false"])
                .args(["-c", "worktree.useRelativePaths=false"])
                .args([
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "wt-leg",
                    wt.to_str().unwrap(),
                ])
                .status()
                .unwrap();
            assert!(wt_status.success());
            chmod_tree(&repo);
            chmod_tree(&wt);
            std::fs::set_permissions(&wt_parent, std::fs::Permissions::from_mode(0o775)).unwrap();

            let turn = super::super::codex_chat_fixture::TURN;
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
            let mut h_stdin = holder.stdin.take().unwrap();
            let mut h_reader = BufReader::new(holder.stdout.take().unwrap());
            writeln!(h_stdin, "echo \"READY:$$\"").unwrap();
            h_stdin.flush().unwrap();
            let mut ready = String::new();
            h_reader.read_line(&mut ready).unwrap();
            let lead_pid: u32 = ready
                .trim()
                .strip_prefix("READY:")
                .unwrap()
                .parse()
                .unwrap();

            let prompt_line =
                format!("{TASK_PREFIX} Turn {turn}. You own this provider's work. Check workspace");
            let req = format!("<USER_REQUEST>\n{prompt_line}\n</USER_REQUEST>");
            let ws_uri = format!("file://{}", repo.display());
            let wt_uri = format!("file://{}", wt.display());
            let subagent_body = json!({"conversationId": child_id, "workspaceUris": [ws_uri]});
            let legacy_body = json!({"conversationId": legacy_child, "workspaceUris": [wt_uri]});
            let preamble = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
            let child_prompt = "inspect crate";
            let legacy_prompt = "inspect legacy wt";
            let lead_rows = vec![
                json!({"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":req}),
                json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"spawning","tool_calls":[
                    {"id":"t1","name":"invoke_subagent","args":{"Prompt":child_prompt,"Role":"Researcher","TypeName":"general"}},
                    {"id":"t2","name":"invoke_subagent","args":{"Prompt":legacy_prompt,"Role":"Legacy WT","TypeName":"general"}}
                ]}),
                json!({"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:00Z","content":format!("Created the following subagent:\n{subagent_body:#}")}),
                json!({"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:01Z","content":format!("Created the following subagent:\n{legacy_body:#}")}),
                json!({"step_index":4,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-11-18T18:01:00Z","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:00Z sender={child_id} priority=HIGH content=PARENT_DELIVERED_SUMMARY\n</SYSTEM_MESSAGE>")}),
            ];
            let child_rows = vec![
                json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:00Z sender={lead_id} content={child_prompt}\n</SYSTEM_MESSAGE>")}),
                json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"OWN_CHILD_DIRECT_REPLY","thinking":"HIDDEN_THINKING","model":"gemini-x","created_at":"2026-11-18T18:00:30Z","input_tokens":7,"cache_read_tokens":3,"output_tokens":2}),
            ];
            let legacy_child_rows = vec![
                json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:01Z sender={lead_id} content={legacy_prompt}\n</SYSTEM_MESSAGE>")}),
                json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"UNPROVED_LEGACY_CHILD_REPLY"}),
            ];
            let write_jsonl = |id: &str, name: &str, rows: &[Value]| {
                let id_dir = private_dir(&brain.join(id));
                let sys_dir = private_dir(&id_dir.join(".system_generated"));
                let logs = private_dir(&sys_dir.join("logs"));
                let p = logs.join(name);
                let mut bytes = Vec::new();
                for r in rows {
                    bytes.extend_from_slice(r.to_string().as_bytes());
                    bytes.push(b'\n');
                }
                std::fs::write(&p, bytes).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            };
            write_jsonl(lead_id, "transcript.jsonl", &lead_rows);
            write_jsonl(lead_id, "transcript_full.jsonl", &lead_rows);
            write_jsonl(child_id, "transcript.jsonl", &child_rows);
            write_jsonl(child_id, "transcript_full.jsonl", &child_rows);
            write_jsonl(legacy_child, "transcript.jsonl", &legacy_child_rows);
            write_jsonl(legacy_child, "transcript_full.jsonl", &legacy_child_rows);

            let spy = SpyPanel::new(repo.clone());
            spy.allow_board_queries.set(true);
            spy.leads.borrow_mut().push(FakeLead {
                window: "@4".into(),
                pane_id: "%4".into(),
                pid: lead_pid,
                active: false,
                provider: "antigravity".into(),
            });
            let screen = format!("> {prompt_line}\n");
            spy.screens.borrow_mut().insert("%4".into(), screen.clone());
            spy.recent_screens.borrow_mut().insert("%4".into(), screen);

            let mut host = FakeHost::new(state);
            let main_owner = terminals::process(spy.main_pid).unwrap();
            let lead_owner = terminals::process(lead_pid).unwrap();
            host.owners.insert(spy.main_pid, main_owner);
            host.owners.insert(lead_pid, lead_owner);

            let participant = crate::native_children::participant_id("antigravity", child_id);
            let legacy_participant =
                crate::native_children::participant_id("antigravity", legacy_child);
            let mut obs = Observations::default();
            let mut report = accepted_codex_scope();

            // Cold + warm accepted refreshes: ZERO legacy child reads, ONE fenced parent scan
            // with ONE bounded usage-suffix read per sample, and ONLY 2 capture-pane calls.
            let panel = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
            let expected_passive_tmux = vec![
                vec![
                    "tmux".to_owned(),
                    "capture-pane".to_owned(),
                    "-p".to_owned(),
                    "-t".to_owned(),
                    "%4".to_owned(),
                ],
                vec![
                    "tmux".to_owned(),
                    "capture-pane".to_owned(),
                    "-p".to_owned(),
                    "-J".to_owned(),
                    "-S".to_owned(),
                    "-100".to_owned(),
                    "-t".to_owned(),
                    "%4".to_owned(),
                ],
            ];
            crate::agy_child_chat::reset_all_agy_counters();
            spy.calls.borrow_mut().clear();
            panel.observe_with_host(&mut obs, &mut report, true, &host);
            assert_eq!(crate::agy_child_chat::legacy_agy_read_index(), (1, 1, 0, 0));
            assert_eq!(crate::agy_child_chat::test_counters(), (2, 1, 1));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 1));
            assert_eq!(crate::agy_child_chat::total_agy_child_reads(), 1);
            assert_eq!(*spy.calls.borrow(), expected_passive_tmux);
            let accepted = report["native_children"].clone();

            // Rejected sightings (not chat-current, pending scope, stopping) make ZERO
            // custody checks, scans or child reads and carry no earlier usage forward.
            for (current, pending, stop) in [
                (false, false, false),
                (true, true, false),
                (true, false, true),
            ] {
                let mut rejected = accepted_codex_scope();
                rejected["collection"]["pending"] = json!(pending);
                spy.stop.store(stop, Ordering::Release);
                crate::agy_child_chat::reset_all_agy_counters();
                panel.observe_with_host(&mut obs, &mut rejected, current, &host);
                spy.stop.store(false, Ordering::Release);
                assert_eq!(crate::agy_child_chat::test_counters(), (0, 0, 0));
                assert_eq!(crate::agy_child_chat::total_agy_child_reads(), 0);
                // The child's row is still painted, just without usage.
                assert!(
                    stop || rejected["native_children"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["participant"] == participant)
                );
                assert!(
                    !rejected["native_children"]
                        .to_string()
                        .contains("native_response_usage")
                );
            }

            // The same accepted sample again: the same rows, one scan, no extra reads.
            crate::agy_child_chat::reset_all_agy_counters();
            spy.calls.borrow_mut().clear();
            panel.observe_with_host(&mut obs, &mut report, true, &host);
            assert_eq!(crate::agy_child_chat::legacy_agy_read_index(), (0, 1, 0, 0));
            assert_eq!(crate::agy_child_chat::test_counters(), (2, 1, 1));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 1));
            assert_eq!(crate::agy_child_chat::total_agy_child_reads(), 1);
            assert_eq!(*spy.calls.borrow(), expected_passive_tmux);
            assert_eq!(report["native_children"], accepted);

            // A Board read expecting the generation the render fence requires:
            // when the lead already left it (another window), the fresh census
            // stops the sighting before custody, scans, reads or chat binding; the
            // matching generation keeps the accepted caps. Each costs one census.
            spy.leads.borrow_mut()[0].window = "@5".into();
            let moved = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop)
                .unwrap()
                .observation_generation_with_host(&host)
                .unwrap();
            spy.leads.borrow_mut()[0].window = "@4".into();
            let current = panel.observation_generation_with_host(&host).unwrap();
            let census: Vec<Vec<String>> = [
                &["tmux", "list-panes", "-s", "-t", "%2", "-F", PANE_FORMAT][..],
                &[
                    "tmux",
                    "display-message",
                    "-p",
                    "-t",
                    "$1",
                    terminals::PROVIDER_SESSION_IDENTITY_FORMAT,
                    ";",
                    "show-options",
                    "-v",
                    "-t",
                    "$1",
                    "@oms_panel_repo",
                ],
            ]
            .map(|call| call.iter().map(|word| (*word).to_owned()).collect())
            .into();
            for (expected, counters, accepted_frame) in
                [(&moved, (0, 0, 0), false), (&current, (2, 1, 1), true)]
            {
                let mut fenced_obs = obs.clone();
                let mut fenced = accepted_codex_scope();
                crate::agy_child_chat::reset_all_agy_counters();
                spy.calls.borrow_mut().clear();
                panel.observe_reading(
                    &mut fenced_obs,
                    &mut fenced,
                    true,
                    &host,
                    |_, _, _| None,
                    Some(expected),
                );
                assert_eq!(crate::agy_child_chat::test_counters(), counters);
                assert_eq!(crate::agy_child_chat::total_agy_child_reads(), counters.2);
                assert_eq!(
                    *spy.calls.borrow(),
                    [expected_passive_tmux.clone(), census.clone()].concat()
                );
                assert_eq!(
                    fenced_obs.chat_binding(&participant).is_some(),
                    accepted_frame
                );
                let rows = &fenced["native_children"];
                assert!(
                    rows.as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["participant"] == participant)
                );
                assert_eq!(
                    rows.to_string().contains("native_response_usage"),
                    accepted_frame
                );
                assert!(!accepted_frame || *rows == accepted);
            }

            // With a Claude lead too, both providers are fenced from one census.
            let claude_pid = 4_194_305; // above Linux's pid_max, so it reads nothing
            spy.leads.borrow_mut().push(FakeLead {
                window: "@5".into(),
                pane_id: "%5".into(),
                pid: claude_pid,
                active: false,
                provider: "claude".into(),
            });
            let uid = rustix::process::getuid().as_raw();
            host.owners
                .insert(claude_pid, owner_entry(claude_pid, "claude-birth", uid));
            let both = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
            let current = both.observation_generation_with_host(&host).unwrap();
            let claude_capture: Vec<String> = ["tmux", "capture-pane", "-p", "-t", "%5"]
                .map(str::to_owned)
                .into();
            for (expected, fence) in [(None, vec![]), (Some(&current), census.clone())] {
                crate::agy_child_chat::reset_all_agy_counters();
                spy.calls.borrow_mut().clear();
                both.observe_reading(
                    &mut obs.clone(),
                    &mut accepted_codex_scope(),
                    true,
                    &host,
                    |_, _, _| None,
                    expected,
                );
                assert_eq!(crate::agy_child_chat::test_counters(), (2, 1, 1));
                assert_eq!(
                    *spy.calls.borrow(),
                    [
                        expected_passive_tmux.clone(),
                        fence,
                        vec![claude_capture.clone()]
                    ]
                    .concat()
                );
            }
            spy.leads.borrow_mut().pop();
            host.owners.remove(&claude_pid);

            let rows = report["native_children"].as_array().unwrap();
            assert_eq!(rows.len(), 2);
            let row_strict = rows
                .iter()
                .find(|r| r["participant"] == participant)
                .unwrap();
            assert_eq!(row_strict["provider"], "antigravity");
            assert_eq!(row_strict["state"], "done");
            assert_eq!(row_strict["native_answer"], "PARENT_DELIVERED_SUMMARY");
            assert_eq!(
                row_strict["native_response_usage"],
                json!({"scope": "latest_child_response",
                       "response": {"model": "gemini-x", "created_at": "2026-11-18T18:00:30Z",
                       "input_tokens": 7, "cached_input_tokens": 3, "output_tokens": 2}})
            );
            let row_legacy = rows
                .iter()
                .find(|r| r["participant"] == legacy_participant)
                .unwrap();
            assert_eq!(row_legacy["state"], "unknown");
            assert_eq!(row_legacy["native_answer"], "");
            assert!(row_legacy.get("native_response_usage").is_none());
            assert!(
                !row_legacy
                    .to_string()
                    .contains("UNPROVED_LEGACY_CHILD_REPLY")
            );

            let binding = obs.chat_binding(&participant).unwrap().clone();
            assert!(matches!(binding.locator, NativeLocator::Antigravity(_)));
            let legacy_binding = obs.chat_binding(&legacy_participant).unwrap().clone();

            // Panel::open_agy_chat returns OwnChat on strict 0700 child with (custody=2, scans=1, reads=1).
            crate::agy_child_chat::reset_all_agy_counters();
            let attempts = AttemptCache::default();
            let open_panel = Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop).unwrap();
            let crate::agy_child_chat::AgyChatOpen::OwnChat {
                history: opened,
                locator,
            } = open_panel.open_agy_chat(&binding, None, &attempts)
            else {
                panic!("expected OwnChat on strict 0700 child");
            };
            assert_eq!(crate::agy_child_chat::test_counters(), (2, 1, 1));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 1));
            assert_eq!(opened, "OWN_CHILD_DIRECT_REPLY");
            assert!(!opened.contains("PARENT_DELIVERED_SUMMARY"));
            assert!(!opened.contains("HIDDEN_THINKING"));

            // recheck_open_agy_chat passes while Turn & inode hold (custody=2, scans=1, reads=0),
            // fails closed when post-fence Turn advances (custody=1, scans=1, reads=0) or child inode swaps,
            // and open_agy_chat fails closed when post-fence Turn advances.
            crate::agy_child_chat::reset_all_agy_counters();
            assert!(open_panel.recheck_open_agy_chat(&binding, &locator, None, &attempts));
            assert_eq!(crate::agy_child_chat::test_counters(), (2, 1, 0));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));

            let new_turn = super::super::codex_chat_fixture::NEW_TURN;
            *spy.next_recent.borrow_mut() = Some(format!(
                "> {TASK_PREFIX} Turn {new_turn}. You own this provider's work. Next\n"
            ));
            crate::agy_child_chat::reset_all_agy_counters();
            assert!(!open_panel.recheck_open_agy_chat(&binding, &locator, None, &attempts));
            assert_eq!(crate::agy_child_chat::test_counters(), (1, 1, 0));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));
            spy.recent_screens
                .borrow_mut()
                .insert("%4".into(), format!("> {prompt_line}\n"));

            *spy.next_recent.borrow_mut() = Some(format!(
                "> {TASK_PREFIX} Turn {new_turn}. You own this provider's work. Next\n"
            ));
            assert!(matches!(
                open_panel.open_agy_chat(&binding, None, &attempts),
                crate::agy_child_chat::AgyChatOpen::Unavailable
            ));
            spy.recent_screens
                .borrow_mut()
                .insert("%4".into(), format!("> {prompt_line}\n"));

            let child_full = brain
                .join(child_id)
                .join(".system_generated/logs/transcript_full.jsonl");
            let orig_child_bytes = std::fs::read(&child_full).unwrap();
            let swap_child = tmp.0.join("swap-child-orch.jsonl");
            std::fs::write(&swap_child, &orig_child_bytes).unwrap();
            std::fs::set_permissions(&swap_child, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::rename(&swap_child, &child_full).unwrap();
            crate::agy_child_chat::reset_all_agy_counters();
            assert!(!open_panel.recheck_open_agy_chat(&binding, &locator, None, &attempts));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));

            // Selecting negative 0775-wt legacy child returns Unbound with 0 child reads.
            crate::agy_child_chat::reset_all_agy_counters();
            assert!(matches!(
                open_panel.open_agy_chat(&legacy_binding, None, &attempts),
                crate::agy_child_chat::AgyChatOpen::Unbound
            ));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));
            assert_eq!(crate::agy_child_chat::total_agy_child_reads(), 0);

            // If .git becomes 0o775, observe's one usage scan stops at custody (1, 0, 0) with
            // 0 reads and no usage, and open_agy_chat returns Unbound (0 reads).
            std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o775))
                .unwrap();
            let mut fallback_obs = Observations::default();
            let mut fallback_report = accepted_codex_scope();
            crate::agy_child_chat::reset_all_agy_counters();
            Panel::load_for_caller(&repo, ROOM, "%2", &spy, &spy.stop)
                .unwrap()
                .observe_with_host(&mut fallback_obs, &mut fallback_report, true, &host);
            assert_eq!(crate::agy_child_chat::test_counters(), (1, 0, 0));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));
            assert!(
                !fallback_report["native_children"]
                    .to_string()
                    .contains("native_response_usage")
            );
            let fallback_binding = fallback_obs.chat_binding(&participant).unwrap().clone();
            crate::agy_child_chat::reset_all_agy_counters();
            assert!(matches!(
                open_panel.open_agy_chat(&fallback_binding, None, &attempts),
                crate::agy_child_chat::AgyChatOpen::Unbound
            ));
            assert_eq!(crate::agy_child_chat::agy_child_read_counts(), (0, 0));
            std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o700))
                .unwrap();

            // When strict 0700 child has no assistant reply ("no-reply") in transcript_full.jsonl,
            // even with UNPROVED_LEGACY_CHILD_REPLY left in transcript.jsonl and PARENT_DELIVERED_SUMMARY
            // in parent, open_agy_chat returns Unavailable (NOT Unbound!).
            let empty_child_rows = vec![
                json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:00Z sender={lead_id} content={child_prompt}\n</SYSTEM_MESSAGE>")}),
                json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","thinking":"ONLY_THINKING"}),
            ];
            write_jsonl(child_id, "transcript.jsonl", &legacy_child_rows);
            write_jsonl(child_id, "transcript_full.jsonl", &empty_child_rows);
            assert!(matches!(
                open_panel.open_agy_chat(&binding, None, &attempts),
                crate::agy_child_chat::AgyChatOpen::Unavailable
            ));

            let _ = holder.kill();
            let _ = holder.wait();
        }
    }
}
