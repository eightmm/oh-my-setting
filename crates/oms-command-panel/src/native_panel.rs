//! Native interactive main panel launch, reopen, split construction and window spawning.
use crate::{
    Error, Output, Result, Services,
    cli::Options,
    json,
    orchestrator::account_binding::{
        self, AccountHost, AccountSelectionReceipt, BindingAnchor, SystemAccountHost,
    },
    parent_only, require, rows, text,
};
use oms_panel_core::{
    NativeCommand, Store, Value, host, release, routing, services::ExecutionRequest, state,
    terminals,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_LIVE_MAINS: usize = 1;
const MAIN_ACCOUNT_START_TIMEOUT: Duration = Duration::from_secs(3);
const MAIN_ACCOUNT_READY: &str = "@oms_panel_main_account_ready";
const MAIN_ACCOUNT_HOMES: &[&str] = &["CODEX_HOME", "CLAUDE_CONFIG_DIR"];
#[derive(Clone, Copy, Debug)]
enum NativePrompt {
    Positional,
    AppendSystem,
    AntigravityInteractive,
}

#[derive(Clone, Copy, Debug)]
enum MainTransport {
    Native(NativePrompt),
    CodexGateway,
}

#[derive(Debug)]
struct MainCapability {
    harness: &'static str,
    owner: &'static str,
    program: &'static str,
    transport: MainTransport,
    requires: &'static [&'static str],
    full_permissions: Option<&'static str>,
    resume_refusal: Option<&'static str>,
}

const MAIN_CAPABILITIES: &[MainCapability] = &[
    MainCapability {
        harness: "codex",
        owner: "codex",
        program: "codex",
        transport: MainTransport::Native(NativePrompt::Positional),
        requires: &["codex"],
        full_permissions: Some("--dangerously-bypass-approvals-and-sandbox"),
        resume_refusal: None,
    },
    MainCapability {
        harness: "claude",
        owner: "claude",
        program: "claude",
        transport: MainTransport::Native(NativePrompt::AppendSystem),
        requires: &["claude"],
        full_permissions: Some("--dangerously-skip-permissions"),
        resume_refusal: None,
    },
    MainCapability {
        harness: "agy",
        owner: "antigravity",
        program: "agy",
        transport: MainTransport::Native(NativePrompt::AntigravityInteractive),
        requires: &["agy"],
        full_permissions: Some("--dangerously-skip-permissions"),
        resume_refusal: Some(
            "Agy resume needs a verified native session binding; start a fresh Agy main",
        ),
    },
    MainCapability {
        harness: "pi",
        owner: "codex",
        program: "oms",
        transport: MainTransport::CodexGateway,
        requires: &["pi", "codex", "oms"],
        full_permissions: None,
        resume_refusal: Some("Pi resume needs a verified Pi session binding; start a new Pi main"),
    },
];

impl MainCapability {
    fn installed(&self) -> bool {
        self.requires.iter().all(|program| which_exists(program))
    }

    fn uses_agy_settings(&self) -> bool {
        matches!(
            self.transport,
            MainTransport::Native(NativePrompt::AntigravityInteractive)
        )
    }

    fn require_installed(&self) -> Result<()> {
        if self.installed() {
            return Ok(());
        }
        if matches!(self.transport, MainTransport::CodexGateway) {
            return Err(Error::new(
                "Pi main needs the Pi CLI and Codex CLI on PATH, and the OMS launcher (Pi uses Codex inference)",
            ));
        }
        let harness = self.harness;
        Err(Error::new(format!(
            "{} main needs the {harness} CLI on PATH",
            capitalize(harness)
        )))
    }

    fn require_resume(&self, resume: bool) -> Result<()> {
        if resume && let Some(reason) = self.resume_refusal {
            return Err(Error::new(reason));
        }
        Ok(())
    }

    fn supports_account(&self) -> bool {
        matches!(
            self.transport,
            MainTransport::Native(NativePrompt::Positional | NativePrompt::AppendSystem)
        )
    }
}

fn main_capability(harness: &str) -> Option<&'static MainCapability> {
    let harness = if harness == "antigravity" {
        "agy"
    } else {
        harness
    };
    MAIN_CAPABILITIES
        .iter()
        .find(|entry| entry.harness == harness)
}

fn required_main_capability(raw: &str) -> Result<&'static MainCapability> {
    main_capability(raw).ok_or_else(|| Error::new("unsupported launch provider"))
}
const NATIVE_CONTEXT_ENV: &[&str] = &[
    "OMS_CAMPAIGN_MANIFEST_SHA256",
    "OMS_OPERATION_ID",
    "OMS_GOAL_ID",
    "OMS_TASK_ID",
    "OMS_RUN_ID",
    "OMS_GOAL_RUN_ID",
    "OMS_PANEL_EXECUTION_ROOT",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "OMS_ROOM_PARTICIPANT",
];
fn admission_error(error: oms_command_peer::Error) -> Error {
    Error {
        code: error.code,
        ..Error::new(error.message)
    }
}
const MAIN_STARTUP_PROMPT: &str = include_str!("main-coordination.txt");
const MAIN_PERMISSIONS_ENV: &str = "OMS_PANEL_MAIN_PERMISSIONS";
pub(crate) const MAIN_PERMISSIONS_REF: &str = "panel_main_permissions";
const ENV_KEYS: &[&str] = &[
    "OMS_AGENT",
    "OMS_PANEL_REPO",
    "OMS_PANEL_ENTRYPOINT",
    "PATH",
    "OMS_ROOM_ID",
    "OMS_ROOM_PARTICIPANT",
    "OMS_ROOM_REPO",
];
pub const PANEL_ENV_PASSTHROUGH: &[&str] = &[
    "OMS_PANEL_NO_ANIMATION",
    "OMS_PANEL_AUTO_SWAP",
    "OMS_PANEL_ATTENTION_BELL",
    "OMS_PANEL_ATTENTION_CMD",
    "OMS_PANEL_ATTENTION_OSC",
    "OMS_PANEL_ATTENTION_DONE",
    "OMS_PANEL_TITLE",
    "OMS_PANEL_COLOR",
    "NO_COLOR",
    "OMS_CODEX_NOTIFY",
    "OMS_CLAUDE_NOTIFY",
    "OMS_MAIN_WRITE_GUARD",
    "CLAUDE_CONFIG_DIR",
    "OMS_QUOTA_WARN_PCT",
    "OMS_ROOM_ID",
    "OMS_ROOM_REPO",
    "OMS_PANEL_HOST",
    "OMS_PANEL_VIEW",
    "OMS_PANEL_THEME",
    "COLORFGBG",
    "OMS_EXECUTION_REPO",
    "OMS_MODEL",
    "OMS_ANTIGRAVITY_SETTINGS",
];

struct MainResolution {
    owner: String,
    model: Option<String>,
    effort: String,
}

struct MainAdmission<'a> {
    capability: &'static MainCapability,
    resolution: MainResolution,
    options: &'a Options,
    permissions: MainPermissions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainPermissions {
    Full,
    Native,
}

impl MainPermissions {
    pub(crate) fn from_env() -> Result<Self> {
        match std::env::var(MAIN_PERMISSIONS_ENV) {
            Ok(mode) => Self::parse(Some(&mode)),
            Err(std::env::VarError::NotPresent) => Ok(Self::Full),
            Err(std::env::VarError::NotUnicode(_)) => Self::parse(None),
        }
    }

    pub(crate) fn parse(mode: Option<&str>) -> Result<Self> {
        match mode {
            Some("full") => Ok(Self::Full),
            Some("native") => Ok(Self::Native),
            _ => Err(Error::new(
                "OMS_PANEL_MAIN_PERMISSIONS must be full or native",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Native => "native",
        }
    }
}

impl<'a> MainAdmission<'a> {
    fn plan(
        capability: &'static MainCapability,
        policy: &routing::Policy,
        options: &'a Options,
    ) -> Result<Self> {
        capability.require_resume(options.has("resume"))?;
        if options.has("account") {
            require(
                capability.supports_account(),
                "--launch --account requires codex or claude",
            )?;
            require(
                !options.has("resume"),
                "--launch --account cannot resume without a verified native session account binding",
            )?;
        }
        Ok(Self {
            capability,
            resolution: resolve_main(policy, capability, options.get("model")),
            options,
            permissions: MainPermissions::from_env()?,
        })
    }

    fn admit(
        capability: &'static MainCapability,
        policy: &routing::Policy,
        options: &'a Options,
    ) -> Result<Self> {
        let main = Self::plan(capability, policy, options)?;
        capability.require_installed()?;
        Ok(main)
    }
}

/// Produce `--dry-run` launch plan JSON without spawning processes or opening panels.
pub fn dry_run_plan(
    repo: &Path,
    source: &Path,
    options: &Options,
    _services: &dyn Services,
    _stop: &AtomicBool,
) -> Result<Output> {
    let capability = options
        .get("launch")
        .map(required_main_capability)
        .transpose()?;
    let split = wants_split(options);
    let policy = load_policy(source)?;
    let main = capability
        .map(|entry| MainAdmission::plan(entry, &policy, options))
        .transpose()?;
    let argv = main
        .as_ref()
        .map(|main| native_command(main, repo, None, None, None));
    let host = if split {
        "tmux"
    } else if options.get("host") == Some("herdr") || host::herdr_active() {
        "herdr"
    } else {
        "inline"
    };
    let room = crate::commands::room(options).unwrap_or_else(|| "new on launch".into());
    let mut plan = json!({
        "schema": 1,
        "kind": "oms-panel-launch",
        "repo": repo,
        "layout": if split { "split" } else { "inline" },
        "host": host,
        "provider": capability.map(|entry| entry.owner),
        "harness": capability.map(|entry| entry.harness),
        "max_live_mains": MAX_LIVE_MAINS,
        "argv": argv,
        "model": main.as_ref().and_then(|main| main.resolution.model.as_deref()),
        "room": room,
        "environment_keys": ENV_KEYS,
        "executes": false,
    });
    if let Some(alias) = options.get("account") {
        plan["account_request"] = json!({
            "id": alias,
            "requires_managed_tmux": true,
        });
    }
    Ok((0, json(&plan)?, vec![]))
}

/// Dispatch `--reopen`, `--spawn-main`, or `--launch` interactive lifecycle commands.
pub fn run(
    repo: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    parent_only()?;
    if options.has("launch") || options.has("spawn-main") {
        oms_command_peer::admission::interactive_state(repo).map_err(admission_error)?;
    }
    if options.has("reopen") {
        return reopen(repo, source, services, stop);
    }
    if let Some(raw) = options.get("spawn-main") {
        return spawn_main(repo, source, raw, options, services, stop);
    }
    if let Some(raw) = options.get("launch") {
        return launch(repo, source, raw, options, services, stop);
    }
    Err(Error::new("unsupported native panel action"))
}

pub(crate) fn default_main(
    repo: &Path,
    cwd: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<Output>> {
    parent_only()?;
    let launch_root = if options.has("repo") { repo } else { cwd };
    let (work_root, is_git) = state::work_repository(launch_root)?;
    if !is_git {
        return Ok(None);
    }
    require(
        state::repository_for(&work_root, false)? == repo,
        "current repository conflicts with the inherited OMS_ROOM_REPO",
    )?;
    if !wants_split(options) {
        let Some(raw) = std::env::var_os("OMS_PANEL_MAIN") else {
            return Ok(None);
        };
        let Some(capability) = environment_main(&raw)? else {
            return Ok(None);
        };
        return launch(repo, source, capability.harness, options, services, stop).map(Some);
    }
    if std::env::var_os("OMS_PANEL_MAIN").is_none() {
        main_picker_foreground(std::io::stdin().as_fd())?;
    }
    oms_command_peer::admission::interactive_state(repo).map_err(admission_error)?;
    let prefix = entry_prefix(source)?;
    let policy = load_policy(source)?;
    let attached = split_panel(
        repo,
        &prefix,
        &policy,
        SplitRequest::ReuseOrPick,
        options,
        services,
        stop,
    )?;
    Ok(attached.then(|| (0, vec![], vec![])))
}

fn reopen(
    repo: &Path,
    source: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let prefix = entry_prefix(source)?;
    let panes = discover_panel_panes(repo, services, stop)?;
    let caller = std::env::var("TMUX_PANE").ok();
    let current = caller
        .as_deref()
        .and_then(|p| panes.iter().find(|row| row.pane == p));
    let candidates: Vec<&PanelPane> = panes
        .iter()
        .filter(|row| row.mode != PaneMode::Watch && current.is_none_or(|c| row.window == c.window))
        .collect();
    require(
        candidates.len() == 1,
        "no unique existing OMS input pane; reopen from the intended OMS window",
    )?;
    reopen_pane(repo, &prefix, candidates[0], false, services, stop)
}

fn reopen_pane(
    repo: &Path,
    prefix: &[String],
    base: &PanelPane,
    exclude_account_homes: bool,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let fresh = discover_panel_panes(repo, services, stop)?;
    require(
        fresh.contains(base),
        "OMS input pane changed; retry after refreshing",
    )?;
    let boards: Vec<&PanelPane> = fresh
        .iter()
        .filter(|row| row.window == base.window && row.mode == PaneMode::Watch)
        .collect();
    if boards.is_empty() {
        let selected = tmux_quiet(
            services,
            repo,
            &[
                "show-option",
                "-w",
                "-v",
                "-t",
                &base.pane,
                "@oms_panel_view",
            ],
            stop,
        )
        .unwrap_or_default();
        if selected.is_empty() || selected == "auto" {
            terminals::tmux(
                services,
                repo,
                &crate::commands::strings(&[
                    "set-option",
                    "-w",
                    "-t",
                    &base.pane,
                    "@oms_panel_view",
                    "graph",
                ]),
                stop,
            )?;
        }
        let room = if base.mode == PaneMode::Native {
            let id = tmux_quiet(
                services,
                repo,
                &[
                    "show-option",
                    "-w",
                    "-v",
                    "-t",
                    &base.pane,
                    "@oms_panel_room",
                ],
                stop,
            )
            .ok_or_else(|| Error::new("native Main room is unavailable"))?;
            crate::identifier(&id)?;
            Some(id)
        } else {
            None
        };
        let cmd = watch_shell(
            repo,
            prefix,
            None,
            Some(&base.session),
            room.as_deref(),
            exclude_account_homes,
        );
        board_split(
            &base.pane,
            repo,
            &cmd,
            base.mode == PaneMode::Native,
            services,
            stop,
        )?;
    }
    terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["select-pane", "-t", &base.pane]),
        stop,
    )?;
    let payload = json!({
        "status": if boards.is_empty() { "reopened" } else { "already_open" },
        "session": base.session,
        "window": base.window,
        "input_pane": base.pane,
        "native_restarted": false,
    });
    Ok((0, json(&payload)?, vec![]))
}

fn spawn_main(
    repo: &Path,
    source: &Path,
    raw_provider: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let capability = required_main_capability(raw_provider)?;
    let prefix = entry_prefix(source)?;
    let policy = load_policy(source)?;
    let main = MainAdmission::admit(capability, &policy, options)?;
    let harness = capability.harness;
    require(
        which_exists("tmux"),
        &format!("starting a main needs tmux and the {harness} CLI on PATH"),
    )?;
    let relay = MainRelay::admit(repo, &prefix, &main)?;

    let store = Store::open(repo)?;
    let start_lock = store.transaction("panel-main-start", Duration::from_secs(3))?;
    let (session, owner) = resolve_or_legacy_session(repo, services, stop)?;
    require(
        owner.as_deref() == Some(repo.to_string_lossy().as_ref()),
        "no OMS panel is open for this checkout; open one with oms panel",
    )?;
    start_lock.check()?;
    let ident = panel_room(repo, &session, options, &start_lock, services, stop)?;
    start_lock.check()?;
    {
        let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
        release_dead_mains(repo, &start_lock, &sess_lock, services, stop)?;
        require(
            live_mains(repo, &session, Some(&ident), None, services, stop)? < MAX_LIVE_MAINS,
            "another main is live or unverified; finish it before starting another",
        )?;
        check_legacy_main(
            repo,
            &session,
            None,
            Some((&start_lock, &sess_lock)),
            services,
            stop,
        )?;
        sess_lock.check()?;
    }

    start_lock.check()?;
    let pane = add_main_window(
        repo, &prefix, &session, &main, &relay, "graph", false, &ident, false, services, stop,
    )?;
    let idx_raw = tmux_quiet(
        services,
        repo,
        &["display-message", "-p", "-t", &pane, "#{window_index}"],
        stop,
    )
    .unwrap_or_default();
    let window_idx = idx_raw.trim().parse::<u64>().ok();
    let report = json!({
        "window": window_idx,
        "provider": harness,
        "owner": capability.owner,
        "room": ident,
    });
    if options.has("json") {
        Ok((0, json(&report)?, vec![]))
    } else {
        let cap = capitalize(harness);
        let win_s = window_idx.map_or_else(|| "?".into(), |n| n.to_string());
        Ok((
            0,
            format!("Started {cap} main in window {win_s} of room {ident}\n").into_bytes(),
            vec![],
        ))
    }
}

fn launch(
    repo: &Path,
    source: &Path,
    raw_provider: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let capability = required_main_capability(raw_provider)?;
    let prefix = entry_prefix(source)?;
    let policy = load_policy(source)?;
    let main = MainAdmission::admit(capability, &policy, options)?;
    require(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "interactive panel needs a terminal; use --json or --dry-run",
    )?;
    if wants_split(options) {
        require(
            which_exists("tmux"),
            "native chat board requires tmux on PATH",
        )?;
        let relay = MainRelay::admit(repo, &prefix, &main)?;
        split_panel(
            repo,
            &prefix,
            &policy,
            SplitRequest::StartNew(main, relay),
            options,
            services,
            stop,
        )?;
        return Ok((0, vec![], vec![]));
    }
    let code = run_native(repo, &main, services, stop, &prefix)?;
    Ok((code, vec![], vec![]))
}

fn wants_split(options: &Options) -> bool {
    let host_opt = options.get("host");
    let layout_opt = options.text("layout", "auto");
    !matches!(host_opt, Some("inline" | "herdr"))
        && !host::herdr_active()
        && (host_opt == Some("tmux")
            || layout_opt == "split"
            || (layout_opt == "auto" && which_exists("tmux")))
}

enum SplitRequest<'a> {
    ReuseOrPick,
    StartNew(MainAdmission<'a>, MainRelay),
}

fn split_panel<'a>(
    repo: &Path,
    prefix: &[String],
    policy: &routing::Policy,
    request: SplitRequest<'a>,
    options: &'a Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    let auto_providers = matches!(&request, SplitRequest::ReuseOrPick);
    let store = Store::open(repo)?;
    let repo_str = repo.to_string_lossy().into_owned();
    let take_start_lock = || store.transaction("panel-main-start", Duration::from_secs(3));
    let requested_room = options.get("room");
    let (mut session, mut owner);

    let (main, relay, start_lock) = match request {
        SplitRequest::StartNew(main, relay) => {
            let start_lock = take_start_lock()?;
            (session, owner) = resolve_checked_session(repo, &repo_str, services, stop)?;
            (main, relay, start_lock)
        }
        SplitRequest::ReuseOrPick => {
            let configured = std::env::var_os("OMS_PANEL_MAIN");
            (session, owner) = resolve_checked_session(repo, &repo_str, services, stop)?;
            if owner.is_some()
                && configured.is_none()
                && let StartupMainProbe::Verified(existing) =
                    verified_startup_main(repo, &session, requested_room, services, stop)?
            {
                let to_board = pick_existing_main(&existing, policy, stop)?;
                if stop.load(Ordering::Acquire) {
                    return Err(main_selection_cancelled());
                }
                let start_lock = take_start_lock()?;
                let (current_session, current_owner) =
                    resolve_checked_session(repo, &repo_str, services, stop)?;
                require(
                    current_session == session && current_owner == owner,
                    "Managed session changed during Main selection; retry",
                )?;
                require(
                    matches!(
                        verified_startup_main(repo, &session, requested_room, services, stop)?,
                        StartupMainProbe::Verified(ref current) if current == &existing
                    ),
                    "Main changed during selection; retry",
                )?;
                start_lock.check()?;
                reopen_pane(repo, prefix, &existing.pane, false, services, stop)?;
                if to_board {
                    let panes = discover_panel_panes(repo, services, stop)?;
                    let mut boards = panes.iter().filter(|pane| {
                        pane.mode == PaneMode::Watch
                            && pane.session == session
                            && pane.window == existing.pane.window
                    });
                    let board = boards
                        .next()
                        .filter(|_| boards.next().is_none())
                        .ok_or_else(|| Error::new("A unique Board could not be verified"))?;
                    verify_startup_board(repo, &existing, board, services, stop)?;
                    start_lock.check()?;
                    terminals::tmux(
                        services,
                        repo,
                        &crate::commands::strings(&["select-pane", "-t", &board.pane]),
                        stop,
                    )?;
                }
                start_lock.check()?;
                terminals::tmux(
                    services,
                    repo,
                    &crate::commands::strings(&["select-window", "-t", &existing.pane.pane]),
                    stop,
                )?;
                drop(start_lock);
                return attach_session(&session).map(|()| true);
            }
            let lock = if owner.is_some() {
                let start_lock = take_start_lock()?;
                (session, owner) = resolve_checked_session(repo, &repo_str, services, stop)?;
                if owner.is_some()
                    && try_reuse_native_main(
                        repo,
                        prefix,
                        &session,
                        requested_room,
                        &start_lock,
                        services,
                        stop,
                    )?
                {
                    drop(start_lock);
                    return attach_session(&session).map(|()| true);
                }
                Some(start_lock)
            } else {
                None
            };
            ensure_no_active_main(repo, &session, owner.is_some(), options, services, stop)?;
            drop(lock);
            let chosen = match configured {
                Some(raw) => environment_main(&raw)?,
                None => {
                    MainPermissions::from_env()?;
                    Some(pick_main(stop)?)
                }
            };
            if stop.load(Ordering::Acquire) {
                return Err(main_selection_cancelled());
            }
            let Some(chosen) = chosen else {
                return Ok(false);
            };
            let start_lock = take_start_lock()?;
            (session, owner) = resolve_checked_session(repo, &repo_str, services, stop)?;
            if owner.is_some()
                && try_reuse_native_main(
                    repo,
                    prefix,
                    &session,
                    requested_room,
                    &start_lock,
                    services,
                    stop,
                )?
            {
                drop(start_lock);
                return attach_session(&session).map(|()| true);
            }
            ensure_no_active_main(repo, &session, owner.is_some(), options, services, stop)?;
            let main = MainAdmission::admit(chosen, policy, options)?;
            let relay = MainRelay::admit(repo, prefix, &main)?;
            (main, relay, start_lock)
        }
    };
    let harness = main.capability.harness;
    let chosen_room = crate::commands::room(options);
    let requested_account = options.get("account");
    if let Some(alias) = requested_account {
        let receipt = resolve_requested_main_account(
            &SystemAccountHost,
            repo,
            &session,
            owner.is_some(),
            &main.resolution,
            alias,
            services,
            stop,
        )?;
        if owner.is_some()
            && try_reuse_native_main_account(
                repo,
                prefix,
                &session,
                harness,
                &receipt,
                options,
                &start_lock,
                &SystemAccountHost,
                services,
                stop,
            )?
        {
            drop(start_lock);
            return attach_session(&session).map(|()| true);
        }
        ensure_no_active_main(repo, &session, owner.is_some(), options, services, stop)?;
    }
    if let Some(resume) = options.get("resume") {
        let bound_rooms =
            native_resume_binding(main.capability.owner, repo, resume, services, stop)?;
        require(
            bound_rooms.len() <= 1,
            "native session is bound to multiple rooms; resume refused",
        )?;
        if let Some(bound) = bound_rooms.keys().next() {
            let selected = chosen_room
                .clone()
                .or_else(|| recorded_room(services, repo, &session, stop));
            if let Some(sel) = selected {
                require(
                    &sel == bound,
                    "native session is bound to another room; resume refused",
                )?;
            }
        }
    }

    if owner.is_some() {
        start_lock.check()?;
        let ident = panel_room(repo, &session, options, &start_lock, services, stop)?;
        start_lock.check()?;
        {
            let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
            release_dead_mains(repo, &start_lock, &sess_lock, services, stop)?;
            require(
                live_mains(repo, &session, Some(&ident), None, services, stop)? < MAX_LIVE_MAINS,
                "another main is live or unverified; finish it before starting another",
            )?;
            check_legacy_main(
                repo,
                &session,
                None,
                Some((&start_lock, &sess_lock)),
                services,
                stop,
            )?;
            sess_lock.check()?;
        }
        start_lock.check()?;
        let pane = add_main_window(
            repo,
            prefix,
            &session,
            &main,
            &relay,
            options.get("view").unwrap_or("auto"),
            options.has("attention-only"),
            &ident,
            auto_providers,
            services,
            stop,
        )?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-window", "-t", &pane]),
            stop,
        )?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-pane", "-t", &pane]),
            stop,
        )?;
        if requested_account.is_some() {
            mark_main_account_ready(repo, &pane, &start_lock, services, stop)?;
        }
    } else {
        {
            let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
            check_legacy_main(
                repo,
                &session,
                None,
                Some((&start_lock, &sess_lock)),
                services,
                stop,
            )?;
            sess_lock.check()?;
        }
        let ident = if let Some(resume) = options.get("resume") {
            let bound = native_resume_binding(main.capability.owner, repo, resume, services, stop)?;
            let candidate = chosen_room.or_else(|| bound.into_keys().next());
            start_lock.check()?;
            ensure_room(repo, candidate.as_deref(), services, stop)?
        } else {
            start_lock.check()?;
            ensure_room(repo, chosen_room.as_deref(), services, stop)?
        };
        let mut cmd = relay.command(&session, Some(&ident));
        if auto_providers {
            cmd = enable_auto_providers(&cmd);
        }
        let mut argv = crate::commands::strings(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            &session,
            "-n",
            harness,
            "-c",
        ]);
        argv.push(relay.execution.as_os_str().into());
        argv.push(cmd.into());
        start_lock.check()?;
        let pane = terminals::tmux(services, repo, &argv, stop)?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&[
                "set-environment",
                "-t",
                &format!("={session}"),
                "OMS_PANEL_SESSION",
                &session,
            ]),
            stop,
        )?;
        // Cosmetic and best effort: a refused option never blocks the launch.
        let status = hud_status_right(prefix, repo);
        for (option, value) in [("status-right-length", "90"), ("status-right", &status)] {
            let _ = tmux_quiet(
                services,
                repo,
                &["set-option", "-t", &format!("={session}:"), option, value],
                stop,
            );
        }
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&[
                "set-environment",
                "-t",
                &format!("={session}"),
                "OMS_ROOM_ID",
                &ident,
            ]),
            stop,
        )?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&[
                "set-option",
                "-t",
                &format!("={session}:"),
                "@oms_panel_repo",
                &repo_str,
            ]),
            stop,
        )?;
        bind_window(
            &pane,
            main.capability,
            options.get("view").unwrap_or("auto"),
            options.has("attention-only"),
            &ident,
            services,
            repo,
            stop,
        )?;
        let watch = watch_shell(
            repo,
            prefix,
            Some(main.capability.owner),
            None,
            Some(&ident),
            requested_account.is_some(),
        );
        board_split(&pane, repo, &watch, true, services, stop)?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-pane", "-t", &pane]),
            stop,
        )?;
        if requested_account.is_some() {
            mark_main_account_ready(repo, &pane, &start_lock, services, stop)?;
        }
    }

    drop(start_lock);
    attach_session(&session).map(|()| true)
}

fn mark_main_account_ready(
    repo: &Path,
    pane: &str,
    start_lock: &oms_panel_core::Transaction<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    start_lock.check()?;
    require(
        crate::commands::valid_target(pane, '%'),
        "invalid native Main pane",
    )?;
    terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["set-option", "-w", "-t", pane, MAIN_ACCOUNT_READY, "1"]),
        stop,
    )?;
    Ok(())
}

fn resolve_checked_session(
    repo: &Path,
    repo_str: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<(String, Option<String>)> {
    let (session, owner) = resolve_or_legacy_session(repo, services, stop)?;
    if let Some(o) = &owner {
        require(
            o == repo_str,
            &format!(
                "tmux session {session} is not this checkout's OMS panel; close it or use --layout inline"
            ),
        )?;
    }
    Ok((session, owner))
}

#[derive(PartialEq, Eq)]
struct StartupMain {
    pane: PanelPane,
    session: terminals::SessionProof,
    process: oms_panel_core::native::reference::process::Owner,
    owner: String,
    harness: String,
    room: String,
    attempt: String,
    member: String,
}

fn startup_native_candidate(
    repo: &Path,
    session: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<PanelPane>> {
    let mut panes = discover_panel_panes(repo, services, stop)?
        .into_iter()
        .filter(|pane| pane.session == session && pane.mode == PaneMode::Native);
    let candidate = panes.next();
    require(
        panes.next().is_none(),
        "multiple native Main panes found; reopen from the intended OMS window",
    )?;
    Ok(candidate)
}

enum StartupMainProbe {
    None,
    Unverified,
    Verified(Box<StartupMain>),
}

fn verified_startup_main(
    repo: &Path,
    session: &str,
    requested_room: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<StartupMainProbe> {
    const FORMAT: &str = "#{session_id}\t#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_owner}\t#{@oms_panel_harness}\t#{@oms_panel_room}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room_participant}\t#{@oms_panel_native_pane}\t.";
    let Some(pane) = startup_native_candidate(repo, session, services, stop)? else {
        return Ok(StartupMainProbe::None);
    };
    let Ok(capture) = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["display-message", "-p", "-t", &pane.pane, FORMAT]),
        stop,
    ) else {
        return Ok(StartupMainProbe::Unverified);
    };
    let fields: Vec<_> = capture.split('\t').collect();
    let [
        id,
        name,
        window,
        pane_id,
        pid,
        "0",
        owner,
        harness,
        room_id,
        attempt,
        member,
        native,
        ".",
    ] = fields.as_slice()
    else {
        return Ok(StartupMainProbe::Unverified);
    };
    if *name != session
        || *window != pane.window
        || *pane_id != pane.pane
        || native != pane_id
        || !main_capability(harness).is_some_and(|capability| capability.owner == *owner)
        || ![room_id, attempt, member]
            .into_iter()
            .all(|value| crate::identifier(value).is_ok())
    {
        return Ok(StartupMainProbe::Unverified);
    }
    require(
        requested_room.is_none_or(|requested| requested == *room_id),
        "existing Main belongs to a different room; existing sessions preserved",
    )?;
    let Ok(session) = terminals::provider_session_proof(services, repo, id, stop) else {
        return Ok(StartupMainProbe::Unverified);
    };
    let Ok(pid) = pid.parse() else {
        return Ok(StartupMainProbe::Unverified);
    };
    let Ok(process) = terminals::process(pid) else {
        return Ok(StartupMainProbe::Unverified);
    };
    if process.uid != rustix::process::getuid().as_raw() {
        return Ok(StartupMainProbe::Unverified);
    }
    let Ok(attempts) = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    ) else {
        return Ok(StartupMainProbe::Unverified);
    };
    let mut live = rows(&attempts).iter().filter(|event| {
        event["attempt_id"] == *attempt
            || (event["tool"] == "panel-main"
                && event["terminal"] != true
                && event["refs"]["panel_room_id"] == *room_id)
    });
    let matched = live.next().is_some_and(|event| {
        event["attempt_id"] == *attempt
            && event["provider"] == *owner
            && event["tool"] == "panel-main"
            && event["terminal"] == false
            && event["refs"]["panel_role"] == "main"
            && event["refs"]["panel_room_id"] == *room_id
            && event["refs"]["panel_harness"] == *harness
            && state::main_participant(event) == *member
            && event["state"]
                .as_str()
                .is_some_and(|phase| state::LIVE_STATES.contains(&phase))
    }) && live.next().is_none();
    if !matched
        || !current_auto_orchestrator_room(repo, room_id, member, owner, services, stop)
        || terminals::process(process.pid).ok().as_ref() != Some(&process)
    {
        return Ok(StartupMainProbe::Unverified);
    }
    Ok(StartupMainProbe::Verified(Box::new(StartupMain {
        pane,
        session,
        process,
        owner: (*owner).into(),
        harness: (*harness).into(),
        room: (*room_id).into(),
        attempt: (*attempt).into(),
        member: (*member).into(),
    })))
}

fn verify_startup_board(
    repo: &Path,
    main: &StartupMain,
    board: &PanelPane,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let format = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_dead}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_repo}";
    let live = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["display-message", "-p", "-t", &board.pane, format]),
        stop,
    )?;
    require(
        live.split('\t').eq([
            main.session.session.as_str(),
            &main.pane.window,
            &board.pane,
            "0",
            "1",
            &main.attempt,
            &main.pane.pane,
            &main.member,
            &main.room,
            repo.to_str().unwrap_or_default(),
        ]),
        "Board binding changed; reconnect to Main and reopen its Board",
    )
}

#[derive(Clone, Debug)]
pub(crate) struct MainSwitchTarget {
    pub(crate) harness: &'static str,
    pub(crate) model: String,
}

pub(crate) fn check_outgoing_main_swap_harness(harness: Option<&str>) -> Result<()> {
    require(
        harness == Some("codex"),
        "safe automatic exit currently requires a Codex Main composer",
    )
}

pub(crate) fn main_switch_targets(source: &Path, current: &str) -> Result<Vec<MainSwitchTarget>> {
    require(
        main_capability(current).is_some(),
        "Current Main harness is unverified",
    )?;
    let targets = available_main_switch_targets(&load_policy(source)?, current);
    require(
        !targets.is_empty(),
        "No installed Main target has a configured model",
    )?;
    Ok(targets)
}

fn available_main_switch_targets(policy: &routing::Policy, current: &str) -> Vec<MainSwitchTarget> {
    MAIN_CAPABILITIES
        .iter()
        .filter(|entry| entry.harness != current && entry.installed())
        .filter_map(|entry| {
            let model = resolve_main(policy, entry, None).model?;
            (!model.is_empty()
                && model.len() <= 160
                && model
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte)))
            .then_some(MainSwitchTarget {
                harness: entry.harness,
                model,
            })
        })
        .collect()
}

fn pick_existing_main(
    main: &StartupMain,
    policy: &routing::Policy,
    stop: &AtomicBool,
) -> Result<bool> {
    let stdin = std::io::stdin();
    let alternatives = available_main_switch_targets(policy, &main.harness)
        .into_iter()
        .map(|entry| entry.harness)
        .collect::<Vec<_>>()
        .join(", ");
    let keep = format!("Continue current Main ({})", main.harness);
    let board = if alternatives.is_empty() {
        "Open the Board".into()
    } else {
        format!("Open the Board to switch Main ({alternatives})")
    };
    pick_number(
        &[&keep, &board],
        stdin.as_fd(),
        stop,
        &mut std::io::stderr(),
    )
    .map(|index| index == 1)
}

fn try_reuse_native_main(
    repo: &Path,
    prefix: &[String],
    session: &str,
    requested_room: Option<&str>,
    start_lock: &oms_panel_core::Transaction<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    if let Some(base) = startup_native_candidate(repo, session, services, stop)? {
        start_lock.check()?;
        check_reattach_room(repo, &base, requested_room, services, stop)?;
        start_lock.check()?;
        reopen_pane(repo, prefix, &base, false, services, stop)?;
        start_lock.check()?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-window", "-t", &base.pane]),
            stop,
        )?;
        return Ok(true);
    }
    if let Some(window) = fallback_live_main_window(repo, session, requested_room, services, stop)?
    {
        start_lock.check()?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-window", "-t", &window]),
            stop,
        )?;
        return Ok(true);
    }
    Ok(false)
}

fn fallback_live_main_window(
    repo: &Path,
    session: &str,
    requested_room: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<String>> {
    let raw = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&[
            "list-windows",
            "-t",
            &format!("={session}"),
            "-F",
            "#{window_id}\t#{@oms_panel_owner}\t#{@oms_panel_room}\t#{@oms_panel_room_participant}\t#{window_name}\t#{@oms_panel_harness}\t#{@oms_provider_session}\t#{@oms_panel_repo}\t.",
        ]),
        stop,
    )?;
    let canon = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let canon_str = canon.to_string_lossy();
    let mut matches = Vec::new();
    for line in raw.replace('\r', "").lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [
            window,
            owner,
            room,
            member,
            name,
            harness,
            provider,
            win_repo,
            ".",
        ] = fields.as_slice()
        else {
            continue;
        };
        if !crate::commands::valid_target(window, '@') {
            continue;
        }
        if crate::orchestrator::PROVIDERS.contains(provider)
            && owner.is_empty()
            && member.is_empty()
            && harness.is_empty()
        {
            continue;
        }
        let native = !member.is_empty()
            || !harness.is_empty()
            || (!owner.is_empty() && *name != "control")
            || main_capability(name).is_some();
        if !native {
            continue;
        }
        if !win_repo.is_empty() && *win_repo != canon_str.as_ref() {
            continue;
        }
        matches.push(((*window).to_owned(), (*room).to_owned()));
    }
    require(
        matches.len() <= 1,
        "multiple native Main panes found; reopen from the intended OMS window",
    )?;
    let Some((window, bound_room)) = matches.into_iter().next() else {
        return Ok(None);
    };
    if let Some(requested) = requested_room {
        crate::identifier(requested)?;
        require(
            !bound_room.is_empty(),
            "existing Main room is unavailable; existing sessions preserved",
        )?;
        require(
            bound_room == requested,
            "existing Main belongs to a different room; existing sessions preserved",
        )?;
        let room = crate::commands::query(
            services,
            NativeCommand::Room,
            repo,
            &["show", "--id", requested, "--exact", "--json"],
            stop,
        )?;
        require(
            room["id"] == requested && room["closed"] == false,
            "selected Main room is unavailable or closed; existing sessions preserved",
        )?;
    }
    Ok(Some(window))
}

#[allow(clippy::too_many_arguments)]
fn resolve_requested_main_account(
    host: &dyn AccountHost,
    repo: &Path,
    session: &str,
    existing_session: bool,
    resolution: &MainResolution,
    alias: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<AccountSelectionReceipt> {
    let global = ["show-environment", "-g"];
    let env = if existing_session {
        let global = terminals::tmux(services, repo, &crate::commands::strings(&global), stop)?;
        let local = terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["show-environment", "-t", &format!("={session}")]),
            stop,
        )?;
        format!("{global}\n{local}")
    } else {
        // A fresh tmux server might not exist yet. Inline admission checks the
        // actual inherited environment after the window has been created.
        tmux_quiet(services, repo, &global, stop).unwrap_or_default()
    };
    account_binding::resolve_main_selection(
        host,
        &resolution.owner,
        resolution.model.as_deref(),
        alias,
        &env,
    )
}

#[derive(PartialEq, Eq)]
struct MainAccountPane {
    session: terminals::SessionProof,
    window: String,
    pane: String,
    pid: u32,
    birth: String,
    room: String,
    attempt: String,
    member: String,
}

impl MainAccountPane {
    fn anchor<'a>(&'a self, repo: &'a Path, provider: &'a str) -> BindingAnchor<'a> {
        BindingAnchor {
            repo,
            room: &self.room,
            main_attempt: &self.attempt,
            main_owner: provider,
            session: &self.session,
            provider,
            pane_id: &self.pane,
            pid: self.pid,
            birth: &self.birth,
        }
    }

    fn same_generation(&self, other: &Self) -> bool {
        self.session == other.session
            && self.window == other.window
            && self.pane == other.pane
            && self.pid == other.pid
            && self.birth == other.birth
            && self.room == other.room
    }
}

#[allow(clippy::too_many_arguments)]
fn main_account_pane(
    repo: &Path,
    session: &str,
    pane: &str,
    provider: &str,
    host: &dyn AccountHost,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<MainAccountPane> {
    const FORMAT: &str = "#{session_name}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_owner}\t#{@oms_panel_harness}\t#{@oms_panel_room}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room_participant}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_account_ready}\t.";
    const UNAVAILABLE: &str = "managed Main pane identity is unavailable";
    require(
        valid_panel_session(session) && crate::commands::valid_target(pane, '%'),
        UNAVAILABLE,
    )?;
    let captured = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["display-message", "-p", "-t", pane, FORMAT]),
        stop,
    )?;
    let fields: Vec<_> = captured.split('\t').collect();
    let [
        name,
        id,
        window,
        actual_pane,
        pid,
        "0",
        owner,
        harness,
        room,
        attempt,
        member,
        native,
        "1",
        ".",
    ] = fields.as_slice()
    else {
        return Err(Error::new(UNAVAILABLE));
    };
    require(
        *name == session
            && *actual_pane == pane
            && crate::commands::valid_target(window, '@')
            && *owner == provider
            && *harness == provider
            && crate::identifier(room).is_ok()
            && ((attempt.is_empty() && member.is_empty() && native.is_empty())
                || (crate::identifier(attempt).is_ok()
                    && crate::identifier(member).is_ok()
                    && *native == pane)),
        UNAVAILABLE,
    )?;
    let session = terminals::session_proof(services, repo, id, stop)?;
    let server = terminals::process(session.server_pid)?;
    let pid = pid.parse::<u32>().map_err(|_| Error::new(UNAVAILABLE))?;
    let owner = host.process_owner(pid)?;
    require(
        server.uid == host.uid()
            && server.birth == session.server_birth
            && pid != 0
            && owner.pid == pid
            && owner.uid == host.uid()
            && !owner.birth.is_empty(),
        UNAVAILABLE,
    )?;
    Ok(MainAccountPane {
        session,
        window: (*window).into(),
        pane: pane.into(),
        pid,
        birth: owner.birth,
        room: (*room).into(),
        attempt: (*attempt).into(),
        member: (*member).into(),
    })
}

fn prove_joined_main(
    repo: &Path,
    pane: &MainAccountPane,
    provider: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    JoinedMain {
        room: &pane.room,
        attempt: &pane.attempt,
        member: &pane.member,
        owner: provider,
        harness: provider,
    }
    .prove(repo, services, stop)
}

struct JoinedMain<'a> {
    room: &'a str,
    attempt: &'a str,
    member: &'a str,
    owner: &'a str,
    harness: &'a str,
}

impl JoinedMain<'_> {
    fn prove(&self, repo: &Path, services: &dyn Services, stop: &AtomicBool) -> Result<()> {
        const UNVERIFIED: &str = "exact live Room Main could not be verified";
        let events = crate::commands::query(
            services,
            NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        let mut relevant = rows(&events).iter().filter(|event| {
            event["attempt_id"] == self.attempt
                || (event["tool"] == "panel-main"
                    && event["refs"]["panel_room_id"] == self.room
                    && event["terminal"] != true)
        });
        let event = relevant
            .next()
            .filter(|event| {
                event["attempt_id"] == self.attempt
                    && event["provider"] == self.owner
                    && event["tool"] == "panel-main"
                    && event["terminal"] == false
                    && event["refs"]["panel_role"] == "main"
                    && event["refs"]["panel_owner"] == self.owner
                    && event["refs"]["panel_harness"] == self.harness
                    && event["refs"]["panel_room_id"] == self.room
                    && event["refs"]
                        .get("panel_room_participant")
                        .map_or(self.member == self.attempt, |member| member == self.member)
            })
            .ok_or_else(|| Error::new(UNVERIFIED))?;
        require(relevant.next().is_none(), UNVERIFIED)?;
        let room = swap_room(repo, self.room, self.member, self.owner, services, stop)?;
        if self.member != self.attempt {
            let swap = rows(&room["swaps"])
                .last()
                .filter(|swap| {
                    swap["participant"] == self.member
                        && swap["to"] == self.owner
                        && swap["harness"] == self.harness
                        && swap["model"] == event["refs"]["panel_model"]
                        && swap["attempt"] != self.attempt
                })
                .ok_or_else(|| Error::new("swapped Main lacks exact Room lineage"))?;
            require(
                rows(&events).iter().any(|prior| {
                    prior["attempt_id"] == swap["attempt"]
                        && prior["tool"] == "panel-main"
                        && prior["provider"] == swap["from"]
                        && prior["refs"]["panel_room_id"] == self.room
                        && state::main_participant(prior) == self.member
                        && prior["terminal"] == true
                        && prior["state"] == "abandoned"
                        && prior["reason_code"] == "main_swapped"
                        && prior["refs"]["panel_native_exit"] == "confirmed"
                        && prior["refs"]["panel_native_exit_code"] == "0"
                }),
                "swapped Main predecessor release is unverified",
            )?;
        }
        require(!stop.load(Ordering::Acquire), UNVERIFIED)
    }
}

#[allow(clippy::too_many_arguments)]
fn try_reuse_native_main_account(
    repo: &Path,
    prefix: &[String],
    session: &str,
    provider: &str,
    receipt: &AccountSelectionReceipt,
    options: &Options,
    start_lock: &oms_panel_core::Transaction<'_>,
    host: &dyn AccountHost,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    let panes = discover_panel_panes(repo, services, stop)?;
    let mut native = panes
        .iter()
        .filter(|pane| pane.session == session && pane.mode == PaneMode::Native);
    let Some(base) = native.next() else {
        return Ok(false);
    };
    require(native.next().is_none(), "multiple native Main panes found")?;
    require(!options.has("task"), "a task requires a fresh Main launch")?;
    require(
        !options.has("model"),
        "a model override requires a fresh Main launch",
    )?;
    let room =
        crate::commands::room(options).or_else(|| recorded_room(services, repo, session, stop));
    let generation = main_account_pane(repo, session, &base.pane, provider, host, services, stop)?;
    require(
        generation.window == base.window
            && !generation.attempt.is_empty()
            && room.as_deref() == Some(&generation.room),
        "requested Room does not match the live native Main",
    )?;
    prove_joined_main(repo, &generation, provider, services, stop)?;
    require(
        account_binding::load_verified_binding(host, "main", &generation.anchor(repo, provider))
            .is_some_and(|binding| {
                binding.verification == account_binding::AccountVerification::Verified
                    && binding.account_pinned
                    && binding.account_id == receipt.account_id
                    && binding.home_digest.is_some()
                    && binding.home_digest == receipt.home_digest
            }),
        "requested account does not match a verified live native Main binding",
    )?;
    start_lock.check()?;
    require(
        main_account_pane(repo, session, &base.pane, provider, host, services, stop)? == generation,
        "managed Main pane changed; retry after refreshing",
    )?;
    reopen_pane(repo, prefix, base, true, services, stop)?;
    start_lock.check()?;
    terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&["select-window", "-t", &base.pane]),
        stop,
    )?;
    Ok(true)
}

fn ensure_no_active_main(
    repo: &Path,
    session: &str,
    owner_exists: bool,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let raw = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let attempts = rows(&raw);
    let non_terminal = attempts
        .iter()
        .filter(|row| row["tool"] == "panel-main" && row["terminal"] != true)
        .count();
    if non_terminal > 0 {
        let classified = release::classify(repo, attempts, services);
        require(
            rows(&classified["releasable"]).len() == non_terminal,
            "another main is live or unverified; finish it before starting another",
        )?;
    }
    if owner_exists {
        let existing_room = if non_terminal == 0 {
            crate::commands::room(options).or_else(|| recorded_room(services, repo, session, stop))
        } else {
            None
        };
        require(
            live_mains(
                repo,
                session,
                existing_room.as_deref(),
                None,
                services,
                stop,
            )? < MAX_LIVE_MAINS,
            "another main is live or unverified; finish it before starting another",
        )?;
    }
    Ok(())
}

fn environment_main(raw: &std::ffi::OsStr) -> Result<Option<&'static MainCapability>> {
    let selector = raw
        .to_str()
        .ok_or_else(|| Error::new("OMS_PANEL_MAIN must be UTF-8"))?
        .trim();
    if selector.is_empty() {
        return Ok(None);
    }
    main_capability(selector)
        .map(Some)
        .ok_or_else(|| Error::new("unsupported OMS_PANEL_MAIN; choose codex, claude, agy or pi"))
}

fn pick_main(stop: &AtomicBool) -> Result<&'static MainCapability> {
    let choices: Vec<_> = MAIN_CAPABILITIES
        .iter()
        .filter(|entry| entry.installed())
        .collect();
    require(
        !choices.is_empty(),
        "no installed Main provider is available",
    )?;
    let stdin = std::io::stdin();
    pick_from(&choices, stdin.as_fd(), stop, &mut std::io::stderr())
}

fn main_picker_foreground(input: BorrowedFd<'_>) -> Result<()> {
    require(
        rustix::termios::tcgetpgrp(input).ok() == Some(rustix::process::getpgrp()),
        "Main selection requires a foreground terminal",
    )
}

fn pick_from<'a>(
    choices: &[&'a MainCapability],
    input: BorrowedFd<'_>,
    stop: &AtomicBool,
    out: &mut dyn Write,
) -> Result<&'a MainCapability> {
    let labels: Vec<_> = choices.iter().map(|provider| provider.harness).collect();
    pick_number(&labels, input, stop, out).map(|index| choices[index])
}

fn pick_number(
    choices: &[&str],
    input: BorrowedFd<'_>,
    stop: &AtomicBool,
    out: &mut dyn Write,
) -> Result<usize> {
    writeln!(out, "Choose a Main provider:").map_err(Error::new)?;
    for (index, label) in choices.iter().enumerate() {
        writeln!(out, "  {}. {}", index + 1, label).map_err(Error::new)?;
    }
    let mut pending = Vec::new();
    loop {
        write!(out, "Selection [1]: ").map_err(Error::new)?;
        out.flush().map_err(Error::new)?;
        let line = read_selection(input, &mut pending, stop)?;
        let selection = line.trim();
        if selection.is_empty() {
            return Ok(0);
        }
        if matches!(selection, "q" | "quit") {
            return Err(main_selection_cancelled());
        }
        if let Ok(index) = selection.parse::<usize>()
            && let Some(index) = index.checked_sub(1).filter(|index| *index < choices.len())
        {
            return Ok(index);
        }
        writeln!(out, "Choose one of the listed numbers.").map_err(Error::new)?;
    }
}

fn main_selection_cancelled() -> Error {
    Error {
        code: 130,
        ..Error::new("Main selection cancelled")
    }
}

/// Returns the next line, keeping bytes after it in `pending`. The oms signal
/// handlers only set `stop` and restart syscalls, so a blocking read would
/// never observe SIGINT/SIGTERM; poll in short slices instead. EOF, including
/// one after an unterminated line, cancels.
fn read_selection(
    input: BorrowedFd<'_>,
    pending: &mut Vec<u8>,
    stop: &AtomicBool,
) -> Result<String> {
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::io::Errno;
    let timeout = crate::wakeup::timespec(Duration::from_millis(100));
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(main_selection_cancelled());
        }
        if let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        let mut fds = [PollFd::from_borrowed_fd(input, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(Error::new(e)),
        }
        if stop.load(Ordering::Acquire) {
            return Err(main_selection_cancelled());
        }
        let mut chunk = [0u8; 256];
        match rustix::io::read(input, &mut chunk) {
            Ok(0) => return Err(main_selection_cancelled()),
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(e) => return Err(Error::new(e)),
        }
    }
}

fn attach_session(session: &str) -> Result<()> {
    let attach_verb = if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = Command::new("tmux")
        .args([attach_verb, "-t", &format!("={session}")])
        .status()
        .map_err(Error::new)?;
    println!("OMS session {session} remains available while its panes are running.");
    require(status.success(), "tmux attach failed")
}

const SWAP_SOURCE_ENV: &str = "OMS_PANEL_MAIN_SWAP_SOURCE";
const SWAP_RECEIPT_MAX: usize = 16 * 1024;
const SWAP_PANE: &str = "#{session_name}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_main_watch}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_owner}\t#{@oms_panel_harness}\t#{@oms_panel_repo}\t#{?#{m/r:[[:cntrl:]],#{pane_start_path}},,#{pane_start_path}}\t.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SwapPane {
    name: String,
    session: String,
    window: String,
    pane: String,
    pid: u32,
    dead: bool,
    board: bool,
    native: String,
    attempt: String,
    member: String,
    room: String,
    owner: String,
    harness: String,
    repo: String,
    execution: String,
}

impl SwapPane {
    fn parse(raw: &str) -> Option<Self> {
        let fields: Vec<_> = raw.split('\t').collect();
        let [
            name,
            session,
            window,
            pane,
            pid,
            dead,
            board,
            native,
            attempt,
            member,
            room,
            owner,
            harness,
            repo,
            execution,
            ".",
        ] = fields.as_slice()
        else {
            return None;
        };
        if !valid_panel_session(name)
            || !crate::commands::valid_target(session, '$')
            || !crate::commands::valid_target(window, '@')
            || !crate::commands::valid_target(pane, '%')
            || !matches!(*dead, "0" | "1")
        {
            return None;
        }
        Some(Self {
            name: (*name).into(),
            session: (*session).into(),
            window: (*window).into(),
            pane: (*pane).into(),
            pid: pid.parse().ok()?,
            dead: *dead == "1",
            board: *board == "1",
            native: (*native).into(),
            attempt: (*attempt).into(),
            member: (*member).into(),
            room: (*room).into(),
            owner: (*owner).into(),
            harness: (*harness).into(),
            repo: (*repo).into(),
            execution: (*execution).into(),
        })
    }

    fn same_slot(&self, other: &Self) -> bool {
        self.name == other.name
            && self.session == other.session
            && self.window == other.window
            && self.pane == other.pane
            && self.repo == other.repo
            && !other.board
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum SwapStage {
    // Old receipts did not distinguish a confirmed draft from an uncertain send.
    Prepared,
    BeforeDraft,
    PastePending,
    DraftConfirmed,
    SubmitPending,
    ExitSent,
    RoomCommitted,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SwapRecord {
    schema: u64,
    stage: SwapStage,
    old_attempt: String,
    room: String,
    member: String,
    provider: String,
    harness: String,
    model: String,
    permissions: String,
    session: terminals::SessionProof,
    session_name: String,
    window: String,
    pane: String,
    old_pid: u32,
    old_birth: String,
    controller_pid: u32,
    controller_birth: String,
    execution: PathBuf,
    handoff: String,
    remain_on_exit: Option<String>,
    #[serde(default)]
    auto_orchestrators: bool,
}

fn swap_record_path(attempt: &str) -> Result<PathBuf> {
    crate::identifier(attempt)?;
    Ok(PathBuf::from(format!(
        ".oms/handoffs/main-swap-{attempt}.json"
    )))
}

fn swap_record(store: &Store, attempt: &str) -> Result<Option<SwapRecord>> {
    store
        .json(&swap_record_path(attempt)?, SWAP_RECEIPT_MAX, true)?
        .map(|v| serde_json::from_value(v).map_err(Error::new))
        .transpose()
}

fn write_swap_record(store: &Store, record: &SwapRecord, first: bool) -> Result<()> {
    store
        .transaction("panel-main-swap-record", Duration::from_secs(2))?
        .write_json(
            &swap_record_path(&record.old_attempt)?,
            &serde_json::to_value(record).map_err(Error::new)?,
            first,
            SWAP_RECEIPT_MAX,
        )?;
    Ok(())
}

fn swap_tmux(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
    args: &[&str],
) -> Result<String> {
    // Use the existing supervised tmux command boundary and output limits.
    terminals::tmux(services, repo, &crate::commands::strings(args), stop)
}

fn read_swap_pane(
    repo: &Path,
    pane: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<SwapPane> {
    require(
        crate::commands::valid_target(pane, '%'),
        "invalid Main swap pane",
    )?;
    let row = swap_tmux(
        repo,
        services,
        stop,
        &["display-message", "-p", "-t", pane, SWAP_PANE],
    )?;
    SwapPane::parse(&row)
        .filter(|row| row.pane == pane)
        .ok_or_else(|| Error::new("managed Main pane generation unavailable"))
}

fn find_swap_source(
    repo: &Path,
    main: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<SwapPane> {
    let canonical = repo
        .to_str()
        .ok_or_else(|| Error::new("Main swap repository must be UTF-8"))?;
    require(
        !canonical.chars().any(char::is_control),
        "invalid Main swap repository",
    )?;
    let refs = &main["refs"];
    let inventory = swap_tmux(repo, services, stop, &["list-panes", "-a", "-F", SWAP_PANE])?;
    let mut matches = inventory
        .lines()
        .filter_map(SwapPane::parse)
        .filter(|pane| {
            !pane.dead
                && !pane.board
                && pane.native == pane.pane
                && pane.repo == canonical
                && pane.attempt == main["attempt_id"]
                && pane.room == refs["panel_room_id"]
                && pane.member == refs["panel_room_participant"]
                && pane.owner == main["provider"]
                && pane.harness == refs["panel_harness"]
        });
    let pane = matches
        .next()
        .ok_or_else(|| Error::new("Main swap source pane unavailable"))?;
    require(
        matches.next().is_none(),
        "Main swap source pane is ambiguous",
    )?;
    if let Ok(caller) = std::env::var("TMUX_PANE") {
        let current = read_swap_pane(repo, &caller, services, stop)?;
        require(
            !current.dead
                && current.session == pane.session
                && current.window == pane.window
                && current.repo == pane.repo
                && current.attempt == pane.attempt
                && (current.pane == pane.pane || current.board),
            "Main swap caller is outside the owning panel",
        )?;
    }
    Ok(pane)
}

fn guarded_swap_tmux(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
    expected: &SwapPane,
    dead: bool,
    preserve_forced_exit_policy: bool,
    command: &[&str],
) -> Result<bool> {
    // Condition and action execute within one tmux command queue. The preceding
    // native process and Room checks remain independent of this local fence.
    let wanted = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        expected.session,
        expected.window,
        expected.pane,
        expected.pid,
        u8::from(dead),
        expected.attempt,
        expected.member,
        expected.room
    );
    let condition = format!(
        "#{{==:#{{session_id}}:#{{window_id}}:#{{pane_id}}:#{{pane_pid}}:#{{pane_dead}}:#{{@oms_panel_main_attempt}}:#{{@oms_panel_room_participant}}:#{{@oms_panel_room}},{wanted}}}"
    );
    let condition = if preserve_forced_exit_policy {
        format!("#{{&&:{condition},#{{==:#{{remain-on-exit}},on}}}}")
    } else {
        condition
    };
    let command = command
        .iter()
        .map(|part| crate::commands::shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let output = swap_tmux(
        repo,
        services,
        stop,
        &[
            "if-shell",
            "-F",
            "-t",
            &expected.pane,
            &condition,
            &command,
            "display-message -p OMS_MAIN_SWAP_FENCE_CHANGED",
        ],
    )?;
    match output.as_str() {
        "" => Ok(true),
        "OMS_MAIN_SWAP_FENCE_CHANGED" => Ok(false),
        _ => Err(Error::new("Main swap tmux action outcome is unknown")),
    }
}

fn pane_remain_on_exit(
    repo: &Path,
    pane: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<String>> {
    let local = swap_tmux(
        repo,
        services,
        stop,
        &["show-options", "-pqv", "-t", pane, "remain-on-exit"],
    )?;
    match local.as_str() {
        "" => Ok(None),
        "on" | "off" | "failed" => Ok(Some(local)),
        _ => Err(Error::new("Main pane exit policy unavailable")),
    }
}

fn restore_swap_pane_policy(
    repo: &Path,
    pane: &SwapPane,
    record: &SwapRecord,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    require(
        terminals::provider_session_proof(services, repo, &pane.session, stop)? == record.session,
        "Main swap tmux session changed",
    )?;
    // Preserve a policy that changed while the swap was in flight.
    if pane_remain_on_exit(repo, &pane.pane, services, stop)?.as_deref() != Some("on") {
        return Ok(false);
    }
    let command = match record.remain_on_exit.as_deref() {
        Some("on") => return Ok(true),
        Some(value @ ("off" | "failed")) => {
            vec![
                "set-option",
                "-p",
                "-t",
                &pane.pane,
                "remain-on-exit",
                value,
            ]
        }
        None => vec!["set-option", "-pu", "-t", &pane.pane, "remain-on-exit"],
        _ => return Err(Error::new("Main pane exit policy unavailable")),
    };
    guarded_swap_tmux(repo, services, stop, pane, false, true, &command)
}

fn swap_room(
    repo: &Path,
    room: &str,
    member: &str,
    provider: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let value = crate::commands::query(
        services,
        NativeCommand::Room,
        repo,
        &["show", "--id", room, "--exact", "--json"],
        stop,
    )?;
    let mut mains = rows(&value["participants"])
        .iter()
        .filter(|r| r["participant"] == member || (r["joined"] == true && r["role"] == "main"));
    require(
        value["id"] == room
            && value["closed"] == false
            && value["successor"].is_null()
            && mains.next().is_some_and(|r| {
                r["participant"] == member
                    && r["provider"] == provider
                    && r["joined"] == true
                    && r["role"] == "main"
                    && (r["parent"].is_null() || r["parent"] == "")
            })
            && mains.next().is_none(),
        "Main swap Room or membership changed",
    )?;
    Ok(value)
}

fn fresh_swap_source(
    repo: &Path,
    pane: &SwapPane,
    record: &SwapRecord,
    main: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let live = read_swap_pane(repo, &pane.pane, services, stop)?;
    let process = terminals::process(live.pid)?;
    let source = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["show", "--attempt", &record.old_attempt, "--json"],
        stop,
    )?;
    require(
        live == *pane
            && !live.dead
            && process.pid == record.old_pid
            && process.uid == rustix::process::getuid().as_raw()
            && process.birth == record.old_birth
            && terminals::provider_session_proof(services, repo, &pane.session, stop)?
                == record.session
            && source["attempt_id"] == record.old_attempt
            && source["tool"] == "panel-main"
            && source["state"] == "working"
            && source["terminal"] == false
            && source["refs"] == main["refs"]
            && refs_alive(&source["refs"], "native"),
        "Main swap source generation or lifecycle changed",
    )?;
    swap_room(repo, &pane.room, &pane.member, &pane.owner, services, stop)?;
    require(!stop.load(Ordering::Acquire), "Main swap interrupted")
}

const SWAP_COMPOSER: &str =
    "#{pane_dead}\t#{pane_in_mode}\t#{cursor_x}\t#{cursor_y}\t#{pane_current_command}\t.";

fn swap_composer(
    repo: &Path,
    pane: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<crate::orchestrator::CodexComposer> {
    crate::orchestrator::CodexComposer::for_main(&swap_tmux(
        repo,
        services,
        stop,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            SWAP_COMPOSER,
            ";",
            "capture-pane",
            "-p",
            "-e",
            "-S",
            "0",
            "-t",
            pane,
        ],
    )?)
}

fn bounded_swap_handoff(room: &Value, old: &SwapPane) -> String {
    let mut result = format!(
        "OMS Room {}\nSource Main {} · {}\nCheck the Room, open tasks, and existing publication receipts before acting. Preserve workers and claims.",
        old.room, old.attempt, old.harness
    );
    for participant in rows(&room["participants"])
        .iter()
        .filter(|v| v["joined"] == true && v["role"] != "main")
        .take(16)
    {
        let id = text(participant, "participant");
        let role = text(participant, "role");
        if crate::identifier(id).is_ok() && crate::identifier(role).is_ok() {
            let line = format!("\n{role} {id}");
            if result.len() + line.len() > 3000 {
                break;
            }
            result.push_str(&line);
        }
    }
    result
}

/// Owns the physical transition while the outer release core reserves Main startup.
pub fn swap_main_host(
    repo: &Path,
    request: &oms_panel_core::release::MainSwapRequest,
    main: &Value,
    source: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let repo = std::fs::canonicalize(repo).map_err(Error::new)?;
    let capability = required_main_capability(&request.harness)?;
    require(
        capability.owner == request.provider
            && capability.installed()
            && !request.model.starts_with('-')
            && !request.model.chars().any(char::is_control),
        "Main swap target is not installed or invalid",
    )?;
    perform_main_swap(&repo, request, main, services, stop, |record| {
        let args = [
            "--host",
            "inline",
            "--launch",
            &request.harness,
            "--model",
            &request.model,
            "--task",
            &record.handoff,
        ];
        let options = Options::parse(&crate::commands::strings(&args))?;
        let policy = load_policy(source)?;
        if policy
            .main
            .get(&request.provider)
            .is_none_or(|preset| preset.model != request.model)
        {
            let catalog =
                oms_panel_core::health::provider_catalog(&repo, &request.provider, services, stop)?;
            require(
                rows(&catalog["providers"]).iter().any(|row| {
                    row["provider"] == request.provider
                        && row["installed"] == true
                        && row["usable"] != false
                        && rows(row.get("catalog_models").unwrap_or(&row["models"]))
                            .iter()
                            .any(|model| model == &request.model)
                }),
                "Main swap model is not in the installed provider catalog",
            )?;
        }
        let mut target = MainAdmission::admit(capability, &policy, &options)?;
        require(
            target.resolution.model.as_deref() == Some(&request.model),
            "Main swap target model is not exact",
        )?;
        target.permissions = MainPermissions::parse(Some(&record.permissions))?;
        let mut relay = MainRelay::admit(&repo, &entry_prefix(source)?, &target)?;
        relay.execution = record.execution.clone();
        let execution = record
            .execution
            .to_str()
            .ok_or_else(|| Error::new("Main swap checkout must be UTF-8"))?;
        relay.environment.extend([
            format!("{SWAP_SOURCE_ENV}={}", record.old_attempt),
            format!("OMS_PANEL_EXECUTION_ROOT={execution}"),
        ]);
        Ok(relay.command(&record.session_name, Some(&record.room)))
    })
}

fn perform_main_swap(
    repo: &Path,
    request: &oms_panel_core::release::MainSwapRequest,
    main: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
    admitted_command: impl FnOnce(&SwapRecord) -> Result<String>,
) -> Result<Value> {
    let store = Store::open(repo)?;
    let pane = find_swap_source(repo, main, services, stop)?;
    check_outgoing_main_swap_harness(Some(pane.harness.as_str()))?;
    let refs = &main["refs"];
    let wrapper = terminals::process(pane.pid)?;
    let native_pid = text(refs, "panel_native_pid")
        .parse::<u32>()
        .map_err(Error::new)?;
    let native = terminals::process(native_pid)?;
    let uid = rustix::process::getuid().as_raw();
    require(
        text(refs, "panel_wrapper_pid").parse::<u32>().ok() == Some(pane.pid)
            && wrapper.uid == uid
            && wrapper.birth == refs["panel_wrapper_identity"]
            && native.uid == uid
            && native.birth == refs["panel_native_identity"]
            && native.pid != wrapper.pid,
        "Main swap source predates verified native custody; reopen it before switching",
    )?;
    let prior = swap_record(&store, &request.attempt_id)?;
    let permissions = MainPermissions::parse(refs[MAIN_PERMISSIONS_REF].as_str())?;
    let permissions = if MainPermissions::from_env()? == MainPermissions::Native {
        MainPermissions::Native
    } else {
        permissions
    };
    let room = swap_room(repo, &pane.room, &pane.member, &pane.owner, services, stop)?;
    let session = terminals::provider_session_proof(services, repo, &pane.session, stop)?;
    let old_execution = Path::new(&pane.execution);
    require(
        old_execution.is_absolute() && !pane.execution.chars().any(char::is_control),
        "Main swap source checkout unavailable",
    )?;
    let execution = state::native_repository(repo, Some(old_execution))?;
    let controller = terminals::process(std::process::id())?;
    if let Some(prior) = &prior {
        require(
            prior.schema == 1
                && matches!(
                    prior.stage,
                    SwapStage::BeforeDraft | SwapStage::DraftConfirmed
                )
                && prior.old_attempt == request.attempt_id
                && prior.room == pane.room
                && prior.member == pane.member
                && prior.session == session
                && prior.session_name == pane.name
                && prior.window == pane.window
                && prior.pane == pane.pane
                && prior.old_pid == wrapper.pid
                && prior.old_birth == wrapper.birth
                && prior.execution == execution,
            "Main swap has unresolved recovery evidence; inspect the panel before retrying",
        )?;
    }
    let remain_on_exit = match &prior {
        Some(prior) => prior.remain_on_exit.clone(),
        None => pane_remain_on_exit(repo, &pane.pane, services, stop)?,
    };
    let mut record = SwapRecord {
        schema: 1,
        stage: SwapStage::BeforeDraft,
        old_attempt: request.attempt_id.clone(),
        room: pane.room.clone(),
        member: pane.member.clone(),
        provider: request.provider.clone(),
        harness: request.harness.clone(),
        model: request.model.clone(),
        permissions: permissions.as_str().into(),
        session,
        session_name: pane.name.clone(),
        window: pane.window.clone(),
        pane: pane.pane.clone(),
        old_pid: wrapper.pid,
        old_birth: wrapper.birth,
        controller_pid: controller.pid,
        controller_birth: controller.birth,
        execution,
        handoff: bounded_swap_handoff(&room, &pane),
        remain_on_exit,
        auto_orchestrators: refs["panel_auto_orchestrators"] == "1",
    };
    let entry = admitted_command(&record)?;
    services.check_main_swap_source(repo, main, stop)?;
    fresh_swap_source(repo, &pane, &record, main, services, stop)?;
    const EXIT: &str = "/exit";
    const GENERATION_CHANGED: &str = "Main pane generation changed before tmux operation";
    let first = swap_composer(repo, &pane.pane, services, stop)?;
    let reuse_draft = prior
        .as_ref()
        .is_some_and(|prior| prior.stage == SwapStage::DraftConfirmed && first.exact_draft(EXIT));
    require(
        reuse_draft || first.ready(),
        "Main must be at an empty idle Codex composer before switching",
    )?;
    if reuse_draft {
        record.stage = SwapStage::DraftConfirmed;
    }
    write_swap_record(&store, &record, prior.is_none())?;
    let submitted = (|| {
        require(
            guarded_swap_tmux(
                repo,
                services,
                stop,
                &pane,
                false,
                false,
                &["set-option", "-p", "-t", &pane.pane, "remain-on-exit", "on"],
            )?,
            GENERATION_CHANGED,
        )?;
        fresh_swap_source(repo, &pane, &record, main, services, stop)?;
        require(
            swap_composer(repo, &pane.pane, services, stop)? == first,
            "Main composer changed before exit draft",
        )?;
        let draft = if reuse_draft {
            first
        } else {
            // Write ahead of the first keystroke. A host crash or ambiguous tmux
            // response leaves a stage that can never be replayed automatically.
            record.stage = SwapStage::PastePending;
            write_swap_record(&store, &record, false)?;
            if !guarded_swap_tmux(
                repo,
                services,
                stop,
                &pane,
                false,
                false,
                &["send-keys", "-t", &pane.pane, "-l", EXIT],
            )? {
                record.stage = SwapStage::BeforeDraft;
                write_swap_record(&store, &record, false)?;
                return Err(Error::new(GENERATION_CHANGED));
            }
            let draft_deadline = Instant::now() + Duration::from_secs(2);
            let mut previous = None;
            let draft = loop {
                let current = swap_composer(repo, &pane.pane, services, stop)?;
                if current.exact_draft(EXIT) && previous.as_ref() == Some(&current) {
                    break current;
                }
                previous = current.exact_draft(EXIT).then_some(current);
                require(
                    Instant::now() < draft_deadline,
                    "Main exit draft unconfirmed; inspect the original pane",
                )?;
                std::thread::sleep(Duration::from_millis(100));
            };
            record.stage = SwapStage::DraftConfirmed;
            write_swap_record(&store, &record, false)?;
            draft
        };
        services.check_main_swap_source(repo, main, stop)?;
        fresh_swap_source(repo, &pane, &record, main, services, stop)?;
        require(
            swap_composer(repo, &pane.pane, services, stop)? == draft,
            "Main composer changed before submitting exit",
        )?;
        record.stage = SwapStage::SubmitPending;
        write_swap_record(&store, &record, false)?;
        if !guarded_swap_tmux(
            repo,
            services,
            stop,
            &pane,
            false,
            false,
            &["send-keys", "-t", &pane.pane, "Enter"],
        )? {
            record.stage = SwapStage::DraftConfirmed;
            write_swap_record(&store, &record, false)?;
            return Err(Error::new(GENERATION_CHANGED));
        }
        record.stage = SwapStage::ExitSent;
        write_swap_record(&store, &record, false)
    })();
    if submitted.is_err()
        && matches!(
            record.stage,
            SwapStage::BeforeDraft | SwapStage::DraftConfirmed
        )
        && swap_record(&store, &record.old_attempt).is_ok_and(|saved| {
            saved.is_some_and(|saved| {
                saved.stage == record.stage
                    && saved.controller_pid == record.controller_pid
                    && saved.controller_birth == record.controller_birth
            })
        })
        && terminals::process(pane.pid)
            .is_ok_and(|process| process.uid == uid && process.birth == record.old_birth)
    {
        let settling = AtomicBool::new(false);
        let _ = restore_swap_pane_policy(repo, &pane, &record, services, &settling);
    }
    submitted?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        require(
            !stop.load(Ordering::Acquire) && Instant::now() < deadline,
            "Old Main exit unconfirmed; original pane and handoff retained",
        )?;
        let current = read_swap_pane(repo, &pane.pane, services, stop)?;
        require(
            pane.same_slot(&current) && current.pid == pane.pid && current.attempt == pane.attempt,
            "Main pane changed while waiting for exit",
        )?;
        if current.dead {
            let row = crate::commands::query(
                services,
                NativeCommand::Events,
                repo,
                &["show", "--attempt", &record.old_attempt, "--json"],
                stop,
            )?;
            require(
                row["state"] == "abandoned"
                    && row["terminal"] == true
                    && row["reason_code"] == "main_swapped"
                    && row["refs"]["panel_wrapper_identity"] == record.old_birth
                    && row["refs"]["panel_native_exit"] == "confirmed"
                    && row["refs"]["panel_native_exit_code"] == "0",
                "Old Main exit lacks the exact swap acknowledgement",
            )?;
            oms_panel_core::release::prove_native_absent(&row)?;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let prove_dead = || -> Result<()> {
        let current = read_swap_pane(repo, &pane.pane, services, stop)?;
        require(
            current.dead
                && pane.same_slot(&current)
                && current.pid == pane.pid
                && current.attempt == pane.attempt
                && terminals::provider_session_proof(services, repo, &pane.session, stop)?
                    == record.session,
            "Dead Main pane changed; handoff retained for recovery",
        )
    };
    prove_dead()?;
    let room =
        services.commit_main_swap(repo, &pane.room, &pane.member, request, deadline, stop)?;
    require(
        room["room"] == pane.room
            && room["participant"] == pane.member
            && room["attempt"] == pane.attempt,
        "Canonical Room swap receipt differs; no replacement started",
    )?;
    record.stage = SwapStage::RoomCommitted;
    write_swap_record(&store, &record, false)?;
    prove_dead()?;
    let execution = record
        .execution
        .to_str()
        .ok_or_else(|| Error::new("Main swap checkout must be UTF-8"))?;
    require(
        guarded_swap_tmux(
            repo,
            services,
            stop,
            &pane,
            true,
            false,
            &["respawn-pane", "-t", &pane.pane, "-c", execution, &entry],
        )?,
        GENERATION_CHANGED,
    )?;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        require(
            !stop.load(Ordering::Acquire) && Instant::now() < deadline,
            "Replacement Main not confirmed; preserve the Room and recover from its retained handoff",
        )?;
        let current = read_swap_pane(repo, &pane.pane, services, stop)?;
        require(
            !current.dead && pane.same_slot(&current),
            "Replacement Main pane stopped or moved; handoff retained",
        )?;
        if current.attempt != pane.attempt
            && current.member == pane.member
            && current.owner == request.provider
            && current.harness == request.harness
            && current.room == pane.room
        {
            let row = crate::commands::query(
                services,
                NativeCommand::Events,
                repo,
                &["show", "--attempt", &current.attempt, "--json"],
                stop,
            )?;
            if row["terminal"] == false
                && row["state"] == "working"
                && row["provider"] == request.provider
                && row["refs"]["panel_room_participant"] == pane.member
                && row["refs"]["panel_model"] == request.model
                && refs_alive(&row["refs"], "native")
                && refs_alive(&row["refs"], "wrapper")
                && text(&row["refs"], "panel_wrapper_pid").parse::<u32>().ok() == Some(current.pid)
            {
                let policy_restored =
                    restore_swap_pane_policy(repo, &current, &record, services, stop)
                        .unwrap_or(false);
                return Ok(
                    json!({"status":"swapped","room":pane.room,"participant":pane.member,
                    "old_attempt":pane.attempt,"attempt":current.attempt,"provider":request.provider,
                    "harness":request.harness,"model":request.model,"pane_policy_restored":policy_restored}),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn refs_alive(refs: &Value, kind: &str) -> bool {
    text(refs, &format!("panel_{kind}_pid"))
        .parse::<u32>()
        .ok()
        .and_then(|pid| terminals::process(pid).ok())
        .is_some_and(|owner| {
            owner.uid == rustix::process::getuid().as_raw()
                && !owner.birth.is_empty()
                && refs[format!("panel_{kind}_identity")] == owner.birth
        })
}

fn replacement_member(
    repo: &Path,
    store: &Store,
    main: &MainAdmission<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<SwapRecord>> {
    let Some(source) = std::env::var_os(SWAP_SOURCE_ENV) else {
        return Ok(None);
    };
    let source = source
        .into_string()
        .map_err(|_| Error::new("invalid Main swap source"))?;
    let record =
        swap_record(store, &source)?.ok_or_else(|| Error::new("Main swap handoff unavailable"))?;
    let self_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let pane = read_swap_pane(repo, &self_pane, services, stop)?;
    require(
        managed_session()
            && record.schema == 1
            && record.stage == SwapStage::RoomCommitted
            && terminals::process(record.controller_pid).is_ok_and(|host| {
                host.uid == rustix::process::getuid().as_raw()
                    && host.birth == record.controller_birth
            })
            && record.old_attempt == source
            && pane.pane == record.pane
            && pane.window == record.window
            && pane.pid == std::process::id()
            && pane.pid != record.old_pid
            && !pane.dead
            && !pane.board
            && pane.native == pane.pane
            && pane.attempt == source
            && pane.member == record.member
            && pane.room == record.room
            && pane.repo == repo.to_str().unwrap_or_default()
            && pane.name == record.session_name
            && std::fs::canonicalize(&pane.execution).is_ok_and(|path| path == record.execution)
            && state::native_repository(repo, None)? == record.execution
            && terminals::provider_session_proof(services, repo, &pane.session, stop)?
                == record.session
            && record.provider == main.resolution.owner
            && record.harness == main.capability.harness
            && main.resolution.model.as_deref() == Some(&record.model)
            && record.permissions == main.permissions.as_str()
            && crate::commands::room(main.options).as_deref() == Some(&record.room)
            && main.options.get("task") == Some(record.handoff.as_str())
            && record.handoff.len() <= 3000,
        "Replacement Main is outside its committed pane and Room custody",
    )?;
    let old = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["show", "--attempt", &source, "--json"],
        stop,
    )?;
    require(
        old["state"] == "abandoned"
            && old["reason_code"] == "main_swapped"
            && old["terminal"] == true
            && old["refs"]["panel_wrapper_identity"] == record.old_birth
            && (old["refs"]["panel_auto_orchestrators"] == "1") == record.auto_orchestrators,
        "Replacement Main source exit proof unavailable",
    )?;
    oms_panel_core::release::prove_native_absent(&old)?;
    prove_replacement_room(repo, &record, services, stop)?;
    Ok(Some(record))
}

fn prove_replacement_room(
    repo: &Path,
    record: &SwapRecord,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let room = swap_room(
        repo,
        &record.room,
        &record.member,
        &record.provider,
        services,
        stop,
    )?;
    require(
        rows(&room["swaps"]).last().is_some_and(|r| {
            r["attempt"] == record.old_attempt
                && r["participant"] == record.member
                && r["to"] == record.provider
                && r["model"] == record.model
                && r["harness"] == record.harness
        }),
        "Replacement Main lacks canonical Room swap lineage",
    )
}

fn prove_replacement_custody(
    repo: &Path,
    record: &SwapRecord,
    attempt: &str,
    wrapper_pid: u32,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    prove_replacement_room(repo, record, services, stop)?;
    let pane = read_swap_pane(repo, &record.pane, services, stop)?;
    require(
        pane.name == record.session_name
            && pane.window == record.window
            && pane.pid == wrapper_pid
            && !pane.dead
            && !pane.board
            && pane.native == pane.pane
            && pane.attempt == attempt
            && pane.member == record.member
            && pane.room == record.room
            && pane.owner == record.provider
            && pane.harness == record.harness
            && pane.repo == repo.to_str().unwrap_or_default()
            && std::fs::canonicalize(&pane.execution).is_ok_and(|path| path == record.execution)
            && terminals::provider_session_proof(services, repo, &pane.session, stop)?
                == record.session
            && terminals::process(record.controller_pid).is_ok_and(|host| {
                host.uid == rustix::process::getuid().as_raw()
                    && host.birth == record.controller_birth
            })
            && !stop.load(Ordering::Acquire),
        "Replacement Main pane changed before native launch",
    )
}

fn record_main_refs(
    repo: &Path,
    attempt: &str,
    refs: &[String],
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let mut args =
        crate::commands::strings(&["usage", "--attempt", attempt, "--actor", "panel-main"]);
    for item in refs {
        args.extend(["--ref".into(), item.into()]);
    }
    services
        .command(NativeCommand::Events, repo, &args, stop)?
        .success()
}

fn swap_exit_dispatched(store: &Store, attempt: &str, room: &str, member: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let Ok(Some(record)) = swap_record(store, attempt) else {
            return false;
        };
        if record.schema != 1
            || record.old_attempt != attempt
            || record.room != room
            || record.member != member
            || record.old_pid != std::process::id()
            || !terminals::process(record.old_pid)
                .is_ok_and(|owner| owner.birth == record.old_birth)
        {
            return false;
        }
        if record.stage == SwapStage::ExitSent {
            return true;
        }
        if !matches!(record.stage, SwapStage::SubmitPending | SwapStage::Prepared)
            || Instant::now() >= deadline
        {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_native(
    repo: &Path,
    main: &MainAdmission<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
    prefix: &[String],
) -> Result<i32> {
    oms_command_peer::admission::interactive_state(repo).map_err(admission_error)?;
    let capability = main.capability;
    let permissions = main.permissions.as_str();
    let harness = capability.harness;
    let options = main.options;
    let resolution = &main.resolution;
    let account_host = SystemAccountHost;
    let requested_account = options.get("account");
    if requested_account.is_some() {
        require(
            managed_session(),
            "--launch --account requires a managed tmux panel",
        )?;
    }
    let execution = state::native_repository(repo, None)?;
    let store = Store::open(repo)?;
    let replacement = replacement_member(repo, &store, main, services, stop)?;
    // Ordinary startup cannot race the released-to-replacement interval. Only
    // the proved replacement uses its controller's already-held reservation.
    let start_lock = replacement
        .is_none()
        .then(|| store.transaction("panel-main-start", MAIN_ACCOUNT_START_TIMEOUT))
        .transpose()?;
    let receipt = requested_account
        .map(|alias| {
            account_binding::resolve_main_selection(
                &account_host,
                harness,
                resolution.model.as_deref(),
                alias,
                "",
            )
        })
        .transpose()?;
    let session_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
    let session = std::env::var("OMS_PANEL_SESSION").unwrap_or_default();
    let resume = options.get("resume");
    let chosen_room = crate::commands::room(options);
    let mut pending_account = None;
    if requested_account.is_some() {
        let pane = std::env::var("TMUX_PANE").unwrap_or_default();
        let identity = main_account_pane(
            repo,
            &session,
            &pane,
            harness,
            &account_host,
            services,
            stop,
        )?;
        require(
            identity.pid == std::process::id()
                && identity.attempt.is_empty()
                && chosen_room.as_deref() == Some(&identity.room),
            "launch is not the pending managed Main for the selected Room",
        )?;
        let room = crate::commands::query(
            services,
            NativeCommand::Room,
            repo,
            &["show", "--id", &identity.room, "--exact", "--json"],
            stop,
        )?;
        require(
            room["id"] == identity.room && room["closed"] == false && room["successor"].is_null(),
            "selected Room is closed, replaced, or unavailable",
        )?;
        pending_account = Some(identity);
    }
    if !session.is_empty() {
        let pending = pending_account
            .as_ref()
            .map(|pane| pane.window.clone())
            .or_else(|| pending_window_id(repo, services, stop));
        check_legacy_main(
            repo,
            &session,
            pending.as_deref(),
            start_lock.as_ref().map(|lock| (lock, &session_lock)),
            services,
            stop,
        )?;
        session_lock.check()?;
    }

    let (room_id, replaces) = if let Some(replacement) = &replacement {
        (replacement.room.clone(), None)
    } else if let Some(pending) = &pending_account {
        (pending.room.clone(), None)
    } else if let Some(res) = resume {
        let bound_rooms = native_resume_binding(&resolution.owner, repo, res, services, stop)?;
        require(
            bound_rooms.len() <= 1,
            "native session is bound to multiple rooms; resume refused",
        )?;
        session_lock.check()?;
        let ident = ensure_room(
            repo,
            chosen_room
                .as_deref()
                .or_else(|| bound_rooms.keys().next().map(String::as_str)),
            services,
            stop,
        )?;
        if let Some(bound) = bound_rooms.keys().next() {
            require(
                bound == &ident,
                "native session is bound to another room; resume refused",
            )?;
        }
        let replaces = bound_rooms.get(&ident).cloned();
        (ident, replaces)
    } else {
        session_lock.check()?;
        (
            ensure_room(repo, chosen_room.as_deref(), services, stop)?,
            None,
        )
    };

    let native_session = resume.map(str::to_owned).or_else(|| {
        matches!(
            capability.transport,
            MainTransport::Native(NativePrompt::AppendSystem)
        )
        .then(new_uuid)
    });
    let label = safe_label(options.get("task"));
    let auto_orchestrators = managed_session()
        && std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_none()
        && !prefix.is_empty()
        && replacement.as_ref().map_or_else(
            || {
                std::env::var_os("OMS_PANEL_AUTO_ORCHESTRATORS").as_deref()
                    == Some(std::ffi::OsStr::new("1"))
            },
            |record| record.auto_orchestrators,
        );
    let mut start_args = vec![
        "start".into(),
        "--provider".into(),
        resolution.owner.clone(),
        "--tool".into(),
        "panel-main".into(),
        "--ref".into(),
        "panel_role=main".into(),
        "--ref".into(),
        format!("panel_owner={}", resolution.owner),
        "--ref".into(),
        format!("panel_harness={harness}"),
        "--ref".into(),
        format!("{MAIN_PERMISSIONS_REF}={permissions}"),
        "--ref".into(),
        format!("panel_effort={}", resolution.effort),
        "--ref".into(),
        format!("panel_label={label}"),
        "--ref".into(),
        "panel_location=repository".into(),
        "--ref".into(),
        format!("panel_room_id={room_id}"),
        "--then".into(),
        "starting".into(),
        "--then".into(),
        "working".into(),
    ];
    if let Some(model) = &resolution.model {
        start_args.extend(["--ref".into(), format!("panel_model={model}")]);
    }
    let native_scheduling = if std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_some() {
        "campaign"
    } else {
        "unmanaged"
    };
    start_args.extend([
        "--ref".into(),
        format!("panel_native_scheduling={native_scheduling}"),
    ]);
    if let Some(sid) = &native_session {
        let digest = oms_panel_core::digest(sid.as_bytes());
        start_args.extend([
            "--ref".into(),
            format!("native_session={sid}"),
            "--ref".into(),
            format!("panel_session_digest={}", &digest[..32]),
        ]);
    }
    start_args.extend([
        "--ref".into(),
        "panel_heartbeat_interval=60".into(),
        "--ref".into(),
        "panel_heartbeat_ttl=300".into(),
    ]);
    let wrapper = terminals::process(std::process::id()).ok();
    if let Some(wrapper) = &wrapper {
        let managed = managed_session();
        let host = if managed { "tmux" } else { "inline" };
        for item in [
            format!("panel_host={host}"),
            format!("panel_wrapper_pid={}", wrapper.pid),
            format!("panel_wrapper_identity={}", wrapper.birth),
        ] {
            start_args.extend(["--ref".into(), item]);
        }
        if managed {
            for item in tmux_host_refs(repo, services, stop) {
                start_args.extend(["--ref".into(), item]);
            }
        }
    }
    if auto_orchestrators {
        start_args.extend(["--ref".into(), "panel_auto_orchestrators=1".into()]);
    }
    if let Some(replacement) = &replacement {
        start_args.extend([
            "--ref".into(),
            format!("panel_room_participant={}", replacement.member),
        ]);
    }
    session_lock.check()?;
    let attempt = event_id(repo, &start_args, services, stop)?;
    let member = replacement
        .as_ref()
        .map_or(attempt.as_str(), |record| record.member.as_str())
        .to_owned();

    let mut join_args = vec![
        "join",
        "--id",
        &room_id,
        "--participant",
        &member,
        "--provider",
        &resolution.owner,
        "--role",
        "main",
    ];
    if let Some(model) = &resolution.model {
        join_args.extend(["--model", model]);
    }
    let label_arg = format!("--label={label}");
    join_args.push(&label_arg);
    if let Some(sid) = &native_session {
        join_args.extend(["--native-session", sid]);
    }
    if let Some(previous) = replaces.as_deref() {
        join_args.extend(["--replaces", previous]);
    }
    join_args.push("--json");
    if replacement.is_none()
        && let Err(err) = services
            .command(
                NativeCommand::Room,
                repo,
                &crate::commands::strings(&join_args),
                stop,
            )
            .and_then(|out| out.success())
    {
        let _ = transition_attempt(
            repo,
            &attempt,
            &["failed"],
            Some("room_join_failed"),
            services,
            stop,
        );
        return Err(err);
    }

    if managed_session()
        && let Ok(pane) = std::env::var("TMUX_PANE")
        && crate::commands::valid_target(&pane, '%')
    {
        let canon = std::fs::canonicalize(repo)
            .unwrap_or_else(|_| repo.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let mut tags = vec![
            ("@oms_panel_main_attempt", attempt.as_str()),
            ("@oms_panel_room", room_id.as_str()),
            ("@oms_panel_native_pane", pane.as_str()),
            ("@oms_panel_room_participant", member.as_str()),
            ("@oms_panel_repo", canon.as_str()),
        ];
        if replacement.is_some() {
            tags.extend([
                ("@oms_panel_owner", resolution.owner.as_str()),
                ("@oms_panel_harness", harness),
                (MAIN_ACCOUNT_READY, ""),
            ]);
        }
        for (opt, val) in tags {
            let result = terminals::tmux(
                services,
                repo,
                &crate::commands::strings(&["set-option", "-w", "-t", &pane, opt, val]),
                stop,
            );
            if replacement.is_some()
                && let Err(error) = result
            {
                let _ = transition_attempt(
                    repo,
                    &attempt,
                    &["failed"],
                    Some("native_swap_custody_lost"),
                    services,
                    &AtomicBool::new(false),
                );
                return Err(error);
            }
        }
    }

    let argv = native_command(
        main,
        repo,
        Some(&room_id),
        Some(&member),
        native_session.as_deref(),
    );
    let mut cmd = Command::new(capability.program);
    cmd.args(&argv[1..])
        .current_dir(&execution)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env(MAIN_PERMISSIONS_ENV, permissions)
        .env("OMS_PANEL_OWNER", &resolution.owner)
        .env("OMS_PANEL_HARNESS", harness)
        .env("OMS_ROOM_ID", &room_id)
        .env("OMS_ROOM_PARTICIPANT", &member)
        .env("OMS_ROOM_REPO", repo)
        .env("OMS_PANEL_MAIN_ATTEMPT", &attempt)
        .env("OMS_PANEL_RESULTS", "1");
    cmd.env_remove("OMS_PANEL_AUTO_ORCHESTRATORS")
        .env_remove(SWAP_SOURCE_ENV);
    apply_main_model_env(&mut cmd, harness, resolution.model.as_deref());
    if matches!(capability.transport, MainTransport::Native(_)) {
        cmd.env("OMS_HOOK_AGENT", &resolution.owner);
    } else {
        cmd.env_remove("OMS_HOOK_AGENT");
    }
    for key in [
        "OMS_ATTEMPT_ID",
        "OMS_PANEL_DISPATCH",
        "OMS_PANEL_EXPLICIT_TARGET",
        "OMS_PANEL_ROLE",
        "OMS_PANEL_PURPOSE",
        "OMS_PANEL_WORKLOAD",
        "OMS_PANEL_LABEL",
        "OMS_ROOM_ADMITTED_PARTICIPANT",
    ] {
        cmd.env_remove(key);
    }
    if let Some(receipt) = &receipt {
        for key in receipt.scrub_env {
            cmd.env_remove(key);
        }
        if let Some((key, home)) = &receipt.home_env {
            cmd.env(key, home);
        }
    }

    let admission = (|| -> Result<_> {
        let bound = if let (Some(receipt), Some(pending), Some(lock)) =
            (&receipt, &pending_account, &start_lock)
        {
            lock.check()?;
            let current = main_account_pane(
                repo,
                &session,
                &pending.pane,
                harness,
                &account_host,
                services,
                stop,
            )?;
            require(
                pending.same_generation(&current) && current.attempt == attempt,
                "managed Main pane changed before native launch",
            )?;
            account_binding::save_binding(
                &account_host,
                "main",
                &current.anchor(repo, harness),
                receipt,
            )
            .map_err(|_| Error::new("private Main account binding could not be saved"))?;
            Some(current)
        } else {
            None
        };
        let call = oms_command_peer::admission::begin_interactive_with_executor(
            oms_command_peer::admission::InteractiveRequest {
                state_root: repo,
                provider: &resolution.owner,
                selected_model: resolution.model.as_deref(),
                account: receipt.as_ref().and_then(|r| r.account_id.as_deref()),
                attempt: Some(&attempt),
                resume_session: resume,
            },
            oms_command_peer::admission::InteractiveContext {
                contract_repo: &execution,
                execution_repo: &execution,
                root: Some(oms_command_peer::process::RootLocator {
                    attempt: &attempt,
                    participant: &member,
                    room_id: &room_id,
                }),
                team: None,
                task_id: None,
            },
            services
                .physical_executor()
                .unwrap_or(&oms_command_peer::process::Native),
            stop,
        )
        .map_err(admission_error)?;
        session_lock.check()?;
        call.fence_with_executor(
            services
                .physical_executor()
                .unwrap_or(&oms_command_peer::process::Native),
            stop,
        )
        .map_err(admission_error)?;
        Ok((call, bound))
    })();
    let (call, bound) = match admission {
        Ok(v) => v,
        Err(error) => {
            let settling = AtomicBool::new(false);
            let states: &[&str] = if stop.load(Ordering::Acquire) || error.code == 130 {
                &["cancelled"]
            } else {
                &["failed"]
            };
            let _ = transition_attempt(
                repo,
                &attempt,
                states,
                Some("native_admission_refused"),
                services,
                &settling,
            );
            return Err(error);
        }
    };
    cmd.env("OMS_STATE_REPO", repo)
        .env("OMS_PROVIDER_CALL_ID", call.call_id())
        .env("OMS_OPERATION_ID", call.operation_id())
        .env_remove("OMS_RESUME_SESSION");
    let mut child = match (|| -> Result<Child> {
        if let Some(expected) = &bound {
            require(
                main_account_pane(
                    repo,
                    &session,
                    &expected.pane,
                    harness,
                    &account_host,
                    services,
                    stop,
                )? == *expected,
                "managed Main pane changed before native launch",
            )?;
            prove_joined_main(repo, expected, harness, services, stop)?;
            require(!stop.load(Ordering::Acquire), "native launch cancelled")?;
        }
        if let Some(record) = &replacement {
            session_lock.check()?;
            prove_replacement_custody(repo, record, &attempt, std::process::id(), services, stop)?;
        }
        cmd.spawn()
            .map_err(|err| Error::new(format!("native launch failed: {err}")))
    })() {
        Ok(c) => c,
        Err(err) => {
            let accounting = call.finish(None, None).map_err(admission_error);
            let settling = AtomicBool::new(false);
            let _ = transition_attempt(
                repo,
                &attempt,
                &["failed"],
                Some("native_launch_failed"),
                services,
                &settling,
            );
            accounting?;
            return Err(err);
        }
    };
    if let Some(expected) = &bound
        && let Err(error) = (|| {
            require(
                main_account_pane(
                    repo,
                    &session,
                    &expected.pane,
                    harness,
                    &account_host,
                    services,
                    stop,
                )? == *expected,
                "managed Main pane changed during native launch",
            )?;
            Ok(())
        })()
    {
        let _ = child.kill();
        let _ = child.wait();
        let accounting = call.finish(None, None).map_err(admission_error);
        let settling = AtomicBool::new(false);
        let _ = transition_attempt(
            repo,
            &attempt,
            &["failed"],
            Some("native_account_custody_lost"),
            services,
            &settling,
        );
        accounting?;
        return Err(error);
    }
    if harness == "claude"
        && !stop.load(Ordering::Acquire)
        && let (Some(expected), Some(receipt)) = (&bound, &receipt)
    {
        account_binding::record_claude_main_child(
            &account_host,
            &expected.anchor(repo, harness),
            receipt,
            child.id(),
        );
    }
    let native = terminals::process(child.id());
    let recorded = native.and_then(|child| {
        record_main_refs(
            repo,
            &attempt,
            &[
                format!("panel_room_participant={member}"),
                format!("panel_native_pid={}", child.pid),
                format!("panel_native_identity={}", child.birth),
            ],
            services,
            stop,
        )
    });
    if replacement.is_some()
        && let Err(error) = recorded
    {
        let _ = child.kill();
        let _ = child.wait();
        let _ = call.finish(None, None);
        let _ = transition_attempt(
            repo,
            &attempt,
            &["failed"],
            Some("native_swap_custody_lost"),
            services,
            &AtomicBool::new(false),
        );
        return Err(error);
    }

    drop(start_lock);
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let joined = JoinedMain {
        room: &room_id,
        attempt: &attempt,
        member: &member,
        owner: &resolution.owner,
        harness,
    };
    let background = if auto_orchestrators && joined.prove(repo, services, stop).is_ok() {
        let done = finished.clone();
        let prefix = prefix.to_vec();
        let repo = repo.to_path_buf();
        let room = room_id.clone();
        let attempt = attempt.clone();
        let member = member.clone();
        let owner = resolution.owner.clone();
        Some(std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut pending: Vec<_> = [
                ("codex", "codex"),
                ("claude", "claude"),
                ("antigravity", "agy"),
            ]
            .into_iter()
            .filter(|(_, binary)| which_exists(binary))
            .collect();
            while !pending.is_empty() && !done.load(Ordering::Acquire) && Instant::now() < deadline
            {
                pending.retain(|(provider, _)| {
                    if done.load(Ordering::Acquire) {
                        return false;
                    }
                    // Reenter the existing orchestrator command so ownership is proved there.
                    let result = Command::new(&prefix[0])
                        .args(&prefix[1..])
                        .args(["panel", "--repo"])
                        .arg(&repo)
                        .args([
                            "--room",
                            &room,
                            "--orchestrator",
                            provider,
                            "--owner",
                            &owner,
                        ])
                        .env("OMS_PANEL_MAIN_ATTEMPT", &attempt)
                        .env("OMS_ROOM_PARTICIPANT", &member)
                        .env(MAIN_PERMISSIONS_ENV, permissions)
                        .env_remove("OMS_PANEL_AUTO_ORCHESTRATORS")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    !result.is_ok_and(|status| status.success())
                });
                if !pending.is_empty() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }))
    } else {
        None
    };
    let status_code = wait_with_heartbeat(&mut child, repo, &attempt, services, stop);
    finished.store(true, Ordering::Release);
    if let Some(background) = background {
        let _ = background.join();
    }
    let accounting = call.finish(None, None).map_err(admission_error);
    let settling = AtomicBool::new(false);
    let confirmed_exit = child.try_wait().ok().flatten().is_some();
    if confirmed_exit {
        let _ = record_main_refs(
            repo,
            &attempt,
            &[
                "panel_native_exit=confirmed".into(),
                format!("panel_native_exit_code={status_code}"),
            ],
            services,
            &settling,
        );
    }
    if confirmed_exit
        && status_code == 0
        && accounting.is_ok()
        && swap_exit_dispatched(&store, &attempt, &room_id, &member)
    {
        transition_attempt(
            repo,
            &attempt,
            &["blocked", "abandoned"],
            Some("main_swapped"),
            services,
            &settling,
        )?;
        return Ok(status_code);
    }
    let final_states: &[&str] = match status_code {
        0 => &["verifying", "review", "done"],
        129 | 130 | 143 => &["cancelled"],
        _ => &["failed"],
    };
    let _ = transition_attempt(
        repo,
        &attempt,
        final_states,
        Some("native_exit"),
        services,
        &settling,
    );
    accounting?;
    Ok(status_code)
}

fn current_auto_orchestrator_room(
    repo: &Path,
    room_id: &str,
    member: &str,
    owner: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> bool {
    swap_room(repo, room_id, member, owner, services, stop).is_ok() && !stop.load(Ordering::Acquire)
}

fn wait_with_heartbeat(
    child: &mut Child,
    repo: &Path,
    attempt: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> i32 {
    let mut last_beat = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return status.code().unwrap_or(130);
            }
            Ok(None) => {
                if last_beat.elapsed() >= Duration::from_secs(60) {
                    let _ = heartbeat_attempt(repo, attempt, services, stop);
                    last_beat = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => return 1,
        }
    }
}

fn heartbeat_attempt(
    repo: &Path,
    attempt: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    services
        .command(
            NativeCommand::Events,
            repo,
            &crate::commands::strings(&[
                "heartbeat",
                "--attempt",
                attempt,
                "--actor",
                "panel-main",
            ]),
            stop,
        )?
        .success()
}

fn event_id(
    repo: &Path,
    args: &[String],
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let out = services.command(NativeCommand::Events, repo, &os_args, stop)?;
    out.success()?;
    let raw = String::from_utf8(out.stdout).map_err(Error::new)?;
    let trimmed = raw.trim();
    if trimmed.starts_with('{')
        && let Ok(val) = oms_panel_core::decode(trimmed.as_bytes(), 1024 * 1024)
    {
        let id = text(&val, "attempt_id");
        if !id.is_empty() {
            return Ok(id.to_owned());
        }
    }
    require(!trimmed.is_empty(), "event start returned no attempt id")?;
    Ok(trimmed.to_owned())
}

fn transition_attempt(
    repo: &Path,
    attempt: &str,
    states: &[&str],
    reason: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let Some((first, rest)) = states.split_first() else {
        return Ok(());
    };
    let mut argv = vec![
        "transition",
        "--attempt",
        attempt,
        "--state",
        *first,
        "--actor",
        "panel-main",
    ];
    for s in rest {
        argv.extend(["--then", *s]);
    }
    if let Some(r) = reason {
        argv.extend(["--reason-code", r]);
    }
    services
        .command(
            NativeCommand::Events,
            repo,
            &crate::commands::strings(&argv),
            stop,
        )?
        .success()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PanelPane {
    session: String,
    window: String,
    pane: String,
    mode: PaneMode,
    signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneMode {
    Control,
    Native,
    Watch,
}

fn discover_panel_panes(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Vec<PanelPane>> {
    let raw = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_dead}\t#{@oms_panel_reader}\t#{pane_start_command}",
        ]),
        stop,
    )?;
    require(
        raw.len() <= 1_048_576,
        "panel discovery exceeds the local limit",
    )?;
    let canon_repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut found = Vec::new();
    for line in raw.lines() {
        let fields: Vec<&str> = line.splitn(6, '\t').collect();
        if fields.len() != 6 {
            continue;
        }
        let (session, window, pane, dead, reader, command) = (
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        );
        if !reader.is_empty()
            || !valid_panel_session(session)
            || dead != "0"
            || !crate::commands::valid_target(window, '@')
            || !crate::commands::valid_target(pane, '%')
        {
            continue;
        }
        if let Some(mode) = match_panel_entry(command, &canon_repo) {
            found.push(PanelPane {
                session: session.into(),
                window: window.into(),
                pane: pane.into(),
                mode,
                signature: oms_panel_core::digest(command.as_bytes()),
            });
        }
    }
    Ok(found)
}

#[cfg(test)]
fn match_panel_command(command: &str, canon_repo: &Path) -> Option<bool> {
    match_panel_entry(command, canon_repo).map(|mode| mode == PaneMode::Watch)
}

fn match_panel_entry(command: &str, canon_repo: &Path) -> Option<PaneMode> {
    let mut words = split_shell_words(command)?;
    if words.len() == 1 {
        words = split_shell_words(&words[0])?;
    }
    if words.first().is_some_and(|w| w == "exec") {
        words.remove(0);
    }
    if words.first().is_some_and(|w| w == "env") {
        words.remove(0);
        while words.first().is_some_and(|word| word == "-u") {
            if !words.get(1).is_some_and(|name| is_env_name(name)) {
                return None;
            }
            words.drain(..2);
        }
        while words.first().is_some_and(|w| is_env_assign(w)) {
            words.remove(0);
        }
    }
    if words.is_empty() {
        return None;
    }
    let head = Path::new(&words[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut idx = if matches!(head, "bash" | "python3") {
        if words.len() < 2 {
            return None;
        }
        let script = Path::new(&words[1]).file_name()?.to_str()?;
        if !matches!(script, "oms" | "panel.sh" | "terminal_panel.py") {
            return None;
        }
        if script == "oms" {
            if words.get(2).map(String::as_str) != Some("panel") {
                return None;
            }
            3
        } else {
            2
        }
    } else if head == "oms" {
        let mut start = 1;
        if words.get(start).map(String::as_str) == Some("--oms-dispatch") {
            if words.get(start + 3).map(String::as_str) != Some("--") {
                return None;
            }
            start += 4;
        }
        if words.get(start).map(String::as_str) != Some("panel") {
            return None;
        }
        start + 1
    } else {
        return None;
    };

    let mut watch = false;
    let mut pane_repo: Option<String> = None;
    let mut launch = None;
    let mut account = false;
    let mut resume = false;
    let value_flags = [
        "--repo",
        "--room",
        "--host",
        "--layout",
        "--view",
        "--owner",
        "--launch",
        "--resume",
        "--model",
        "--task",
        "--count",
        "--account",
    ];
    while idx < words.len() {
        let word = words[idx].as_str();
        let (key, inline) = word
            .split_once('=')
            .map_or((word, None), |(key, value)| (key, Some(value)));
        idx += 1;
        if key == "--watch" && inline.is_none() {
            watch = true;
        } else if matches!(key, "--no-animation" | "--attention-only") && inline.is_none() {
            continue;
        } else if value_flags.contains(&key) {
            let val = if let Some(value) = inline {
                value
            } else {
                let value = words.get(idx)?;
                if value.starts_with("--") {
                    return None;
                }
                idx += 1;
                value
            };
            if key == "--repo" {
                pane_repo = Some(val.to_owned());
            } else if key == "--launch" {
                if launch.replace(main_capability(val)?).is_some() {
                    return None;
                }
            } else if key == "--account" {
                if account || !crate::cli::valid_account_id(val) {
                    return None;
                }
                account = true;
            } else if key == "--resume" {
                resume = true;
            }
        } else {
            return None;
        }
    }
    let mode = match (launch, watch) {
        (Some(main), false) => {
            main.require_resume(resume).ok()?;
            if account && (!main.supports_account() || resume) {
                return None;
            }
            PaneMode::Native
        }
        (None, true) if !account => PaneMode::Watch,
        (None, false) if !account => PaneMode::Control,
        _ => return None,
    };
    let resolved = std::fs::canonicalize(pane_repo?).ok()?;
    if resolved == canon_repo {
        Some(mode)
    } else {
        None
    }
}

fn split_shell_words(input: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            Some('"') => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' {
                    cur.push(chars.next()?);
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                ' ' | '\t' | '\n' => {
                    if in_word {
                        out.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    cur.push(chars.next()?);
                    in_word = true;
                }
                _ => {
                    cur.push(c);
                    in_word = true;
                }
            },
            _ => unreachable!(),
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

fn is_env_assign(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    is_env_name(name)
}
fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn board_split(
    pane: &str,
    repo: &Path,
    command: &str,
    main_watch: bool,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let split = main_watch
        .then(|| {
            let geometry = tmux_quiet(
                services,
                repo,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "#{window_panes}\t#{window_zoomed_flag}\t#{window_width}\t#{window_height}",
                ],
                stop,
            )?;
            let ("1", rest) = geometry.split_once('\t')? else {
                return None;
            };
            let ("0", size) = rest.split_once('\t')? else {
                return None;
            };
            let (width, height) = size.split_once('\t')?;
            crate::layout::initial_board_split(
                usize::from(width.parse::<u16>().ok()?),
                usize::from(height.parse::<u16>().ok()?),
            )
        })
        .flatten()
        .map(|(direction, cells)| (direction, cells.to_string()));
    let (direction, cells) = split.as_ref().map_or(("-h", "28%"), |(direction, cells)| {
        (*direction, cells.as_str())
    });
    let mut argv = crate::commands::strings(&[
        "split-window",
        "-f",
        direction,
        "-b",
        "-l",
        cells,
        "-t",
        pane,
        "-c",
    ]);
    argv.push(repo.into());
    argv.push(command.into());
    if main_watch {
        argv.splice(1..1, crate::commands::strings(&["-P", "-F", "#{pane_id}"]));
    }
    let watch_pane = terminals::tmux(services, repo, &argv, stop)?;
    if main_watch && crate::commands::valid_target(watch_pane.trim(), '%') {
        let _ = terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&[
                "set-option",
                "-p",
                "-t",
                watch_pane.trim(),
                "@oms_panel_main_watch",
                "1",
            ]),
            stop,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_main_window(
    repo: &Path,
    prefix: &[String],
    session: &str,
    main: &MainAdmission<'_>,
    relay: &MainRelay,
    view: &str,
    attention_only: bool,
    room_id: &str,
    auto_providers: bool,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let mut cmd = relay.command(session, Some(room_id));
    if auto_providers {
        cmd = enable_auto_providers(&cmd);
    }
    let mut argv = crate::commands::strings(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("={session}:"),
        "-n",
        main.capability.harness,
        "-c",
    ]);
    argv.push(relay.execution.as_os_str().into());
    argv.push(cmd.into());
    let pane = terminals::tmux(services, repo, &argv, stop)?;
    bind_window(
        &pane,
        main.capability,
        view,
        attention_only,
        room_id,
        services,
        repo,
        stop,
    )?;
    let watch = watch_shell(
        repo,
        prefix,
        Some(main.capability.owner),
        None,
        Some(room_id),
        main.options.has("account"),
    );
    board_split(&pane, repo, &watch, true, services, stop)?;
    let _ = tmux_quiet(
        services,
        repo,
        &[
            "set-option",
            "-t",
            &format!("={session}"),
            "@oms_panel_notice",
            "",
        ],
        stop,
    );
    Ok(pane)
}

fn enable_auto_providers(command: &str) -> String {
    command.replacen(
        "'OMS_STATE_REPO=",
        "'OMS_PANEL_AUTO_ORCHESTRATORS=1' 'OMS_STATE_REPO=",
        1,
    )
}

#[allow(clippy::too_many_arguments)]
fn bind_window(
    pane: &str,
    capability: &MainCapability,
    view: &str,
    attention_only: bool,
    room_id: &str,
    services: &dyn Services,
    repo: &Path,
    stop: &AtomicBool,
) -> Result<()> {
    let board_view = if view == "auto" { "graph" } else { view };
    let att = if attention_only { "1" } else { "0" };
    for (k, v) in [
        ("@oms_panel_owner", capability.owner),
        ("@oms_panel_harness", capability.harness),
        ("@oms_panel_view", board_view),
        ("@oms_panel_room", room_id),
        ("@oms_panel_attention", att),
    ] {
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["set-option", "-w", "-t", pane, k, v]),
            stop,
        )?;
    }
    Ok(())
}

struct MainRelay {
    execution: PathBuf,
    environment: Vec<String>,
    argv: Vec<String>,
}

impl MainRelay {
    fn admit(repo: &Path, prefix: &[String], main: &MainAdmission<'_>) -> Result<Self> {
        let (environment, argv) = Self::parts(repo, prefix, main)?;
        Ok(Self {
            execution: state::native_repository(repo, None)?,
            environment,
            argv,
        })
    }

    fn parts(
        repo: &Path,
        prefix: &[String],
        main: &MainAdmission<'_>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let state = repo
            .to_str()
            .ok_or_else(|| Error::new("tmux launch requires a UTF-8 state path"))?;
        let mut parts = vec!["env".into()];
        for name in [
            "OMS_STATE_REPO",
            "OMS_ROOM_REPO",
            "OMS_ROOM_ID",
            SWAP_SOURCE_ENV,
            "OMS_PANEL_AUTO_ORCHESTRATORS",
        ]
        .iter()
        .chain(NATIVE_CONTEXT_ENV)
        {
            parts.extend(["-u".into(), (*name).into()]);
        }
        let keep_agy = main.capability.uses_agy_settings();
        let exclude_account_homes = main.options.has("account");
        parts.extend(
            panel_env_prefix_for(keep_agy, exclude_account_homes)
                .into_iter()
                .skip(1),
        );
        parts.push(format!("OMS_STATE_REPO={state}"));
        parts.push(format!("OMS_ROOM_REPO={state}"));
        parts.push(format!(
            "{MAIN_PERMISSIONS_ENV}={}",
            main.permissions.as_str()
        ));
        for name in NATIVE_CONTEXT_ENV {
            if exclude_account_homes && MAIN_ACCOUNT_HOMES.contains(name) {
                continue;
            }
            if let Some(value) = std::env::var_os(name) {
                let value = value.into_string().map_err(|_| {
                    Error::new(format!(
                        "{name} is not UTF-8; native launch context cannot be relayed"
                    ))
                })?;
                parts.push(format!("{name}={value}"));
            }
        }
        let mut argv = prefix.to_vec();
        argv.extend([
            "panel".into(),
            "--repo".into(),
            repo.to_string_lossy().into_owned(),
            "--host".into(),
            "inline".into(),
            "--launch".into(),
            main.capability.harness.into(),
        ]);
        for key in ["resume", "model", "task", "account"] {
            if let Some(v) = main.options.get(key) {
                let flag = format!("--{key}");
                if v.starts_with("--") {
                    argv.push(format!("{flag}={v}"));
                } else {
                    argv.extend([flag, v.into()]);
                }
            }
        }
        Ok((parts, argv))
    }

    fn command(&self, session: &str, room: Option<&str>) -> String {
        let mut parts = self.environment.clone();
        parts.push(format!("OMS_PANEL_SESSION={session}"));
        if let Some(room) = room {
            parts.push(format!("OMS_ROOM_ID={room}"));
        }
        parts.extend_from_slice(&self.argv);
        format!(
            "exec {}",
            parts
                .iter()
                .map(|p| crate::commands::shell_quote(p))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

fn watch_shell(
    repo: &Path,
    prefix: &[String],
    owner: Option<&str>,
    session: Option<&str>,
    room: Option<&str>,
    exclude_account_homes: bool,
) -> String {
    let mut parts = vec!["env".into()];
    if exclude_account_homes {
        for key in MAIN_ACCOUNT_HOMES {
            parts.extend(["-u".into(), (*key).into()]);
        }
    }
    parts.extend(
        panel_env_prefix_for(
            owner.is_none_or(|o| o == "antigravity" || o == "agy"),
            exclude_account_homes,
        )
        .into_iter()
        .skip(1),
    );
    if let Some(s) = session {
        parts.push(format!("OMS_PANEL_SESSION={s}"));
    }
    parts.extend_from_slice(prefix);
    parts.extend([
        "panel".into(),
        "--repo".into(),
        repo.to_string_lossy().into_owned(),
        "--watch".into(),
    ]);
    if let Some(owner) = owner {
        parts.extend(["--owner".into(), owner.into()]);
    }
    if let Some(room) = room {
        parts.extend(["--room".into(), room.into()]);
    }
    format!(
        "exec {}",
        parts
            .iter()
            .map(|p| crate::commands::shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// Return `(--oms-dispatch, <source>, <rev>, --)` args when `exe` is a verified
/// staged payload under `<source>/target/oms-native/<rev>/oms`, so child panes
/// carry source authority rather than failing as bare direct binary calls.
pub(crate) fn staged_dispatch_args(source: &Path, exe: &Path) -> Option<(String, String)> {
    if exe.file_name().and_then(|v| v.to_str()) != Some("oms") {
        return None;
    }
    let rev_dir = exe.parent()?;
    let rev = rev_dir.file_name().and_then(|v| v.to_str())?;
    if rev.len() != 40 || !rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let native_dir = rev_dir.parent()?;
    if native_dir.file_name().and_then(|v| v.to_str()) != Some("oms-native") {
        return None;
    }
    let canon_source = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let canon_parent = std::fs::canonicalize(native_dir).ok()?;
    if canon_parent != canon_source.join("target/oms-native") {
        return None;
    }
    let is_file = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !is_file(&rev_dir.join("manifest.json")) || !is_file(exe) {
        return None;
    }
    Some((canon_source.to_string_lossy().into_owned(), rev.to_owned()))
}

fn entry_prefix_for(source: &Path, entry: &crate::executable::Entry) -> Vec<String> {
    let mut prefix = Vec::new();
    if entry.script {
        prefix.push("bash".into());
    }
    prefix.push(entry.path.to_string_lossy().into_owned());
    if !entry.script
        && let Some((dispatch_source, rev)) = staged_dispatch_args(source, &entry.path)
    {
        prefix.extend(["--oms-dispatch".into(), dispatch_source, rev, "--".into()]);
    }
    prefix
}

fn entry_prefix(source: &Path) -> Result<Vec<String>> {
    let entry = crate::executable::resolve(source)
        .ok_or_else(|| Error::new("oms panel executable unavailable"))?;
    Ok(entry_prefix_for(source, &entry))
}

/// Apply or scrub `OMS_MODEL` and `OMS_ANTIGRAVITY_SETTINGS` for a spawned main process.
pub fn apply_main_model_env(cmd: &mut Command, harness: &str, resolved_model: Option<&str>) {
    if main_capability(harness).is_some_and(MainCapability::uses_agy_settings) {
        if let Some(m) = resolved_model {
            cmd.env("OMS_MODEL", m);
        } else {
            cmd.env_remove("OMS_MODEL");
        }
    } else {
        cmd.env_remove("OMS_MODEL");
        cmd.env_remove("OMS_ANTIGRAVITY_SETTINGS");
    }
}

fn panel_env_prefix_for(keep_agy_model: bool, exclude_account_homes: bool) -> Vec<String> {
    let mut out = vec!["env".into()];
    for name in PANEL_ENV_PASSTHROUGH {
        if exclude_account_homes && MAIN_ACCOUNT_HOMES.contains(name)
            || (!keep_agy_model && (*name == "OMS_MODEL" || *name == "OMS_ANTIGRAVITY_SETTINGS"))
        {
            continue;
        }
        if let Ok(val) = std::env::var(name) {
            out.push(format!("{name}={val}"));
        }
    }
    out
}

fn native_command(
    main: &MainAdmission<'_>,
    repo: &Path,
    room_id: Option<&str>,
    member: Option<&str>,
    session_id: Option<&str>,
) -> Vec<String> {
    let capability = main.capability;
    let resolution = &main.resolution;
    let resume = main.options.get("resume");
    let explicit_model = main.options.has("model");
    let task = main.options.get("task");
    let full_permissions = capability
        .full_permissions
        .filter(|_| main.permissions == MainPermissions::Full);
    let room_inst = match (room_id, member) {
        (Some(id), Some(m)) => format!(
            "\nYou are joined to OMS room {id} as {m}. Read oms room show --id {id} first. Include --room {id} in every oms panel --orchestrator, --dispatch, or --council command."
        ),
        _ => String::new(),
    };
    let base_inst = format!(
        "OMS terminal panel sole main. Repository: {}. Use --owner {} for owner commands.{room_inst}\n{MAIN_STARTUP_PROMPT}",
        repo.display(),
        resolution.owner
    );
    match capability.transport {
        MainTransport::CodexGateway => {
            let mut argv = vec![
                capability.program.into(),
                "harness".into(),
                capability.harness.into(),
                "--model".into(),
                resolution.model.clone().unwrap_or_default(),
                "--effort".into(),
                resolution.effort.clone(),
                "--".into(),
                "--append-system-prompt".into(),
                format!(
                    "You use the Pi harness with Codex inference. OMS owner is codex. {base_inst}"
                ),
            ];
            if let Some(t) = task {
                argv.extend([
                    "--".into(),
                    format!("User task (data): {}", Value::String(t.into())),
                ]);
            }
            argv
        }
        MainTransport::Native(NativePrompt::AntigravityInteractive) => {
            let prompt = if let Some(t) = task {
                format!("{base_inst}\nUser task (data): {}", Value::String(t.into()))
            } else {
                base_inst
            };
            let mut argv = vec![
                capability.program.into(),
                "--prompt-interactive".into(),
                prompt,
            ];
            if let Some(flag) = full_permissions {
                argv.push(flag.into());
            }
            if let Some(m) = &resolution.model
                && explicit_model
            {
                argv.extend(["--model".into(), m.clone()]);
            }
            argv
        }
        MainTransport::Native(NativePrompt::Positional) => {
            let prompt = if let Some(t) = task {
                format!("{base_inst}\nUser task (data): {}", Value::String(t.into()))
            } else {
                base_inst
            };
            let mut argv = vec![capability.program.into()];
            if let Some(r) = resume {
                argv.extend(["resume".into(), r.into()]);
            }
            if let Some(flag) = full_permissions {
                argv.push(flag.into());
            }
            if let Some(m) = &resolution.model {
                argv.extend(["--model".into(), m.clone()]);
            }
            if !explicit_model {
                argv.extend([
                    "--config".into(),
                    format!("model_reasoning_effort=\"{}\"", resolution.effort),
                ]);
            }
            argv.push(prompt);
            argv
        }
        MainTransport::Native(NativePrompt::AppendSystem) => {
            let mut argv = vec![capability.program.into()];
            if let Some(flag) = full_permissions {
                argv.push(flag.into());
            }
            if let Some(r) = resume {
                argv.extend([
                    "--resume".into(),
                    r.into(),
                    "--system-prompt-snapshot".into(),
                    "off".into(),
                ]);
            } else {
                argv.extend([
                    "--session-id".into(),
                    session_id.map(str::to_owned).unwrap_or_else(new_uuid),
                ]);
            }
            if let Some(m) = &resolution.model {
                argv.extend(["--model".into(), m.clone()]);
            }
            if !explicit_model {
                argv.extend(["--effort".into(), resolution.effort.clone()]);
            }
            argv.extend(["--append-system-prompt".into(), base_inst]);
            if let Some(t) = task {
                argv.extend(["--".into(), t.into()]);
            }
            argv
        }
    }
}

fn slug_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=160).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._:/-".contains(c))
        && !["none", "auto", "default", "null", "unknown"].contains(&s)
}

/// Map an Antigravity display name (or lowercase slug) to its canonical `agy` CLI slug.
pub fn normalize_antigravity_model(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().any(char::is_control) {
        return None;
    }
    if slug_ok(s) {
        return Some(s.to_owned());
    }
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "orchestrator (carbon)" => return Some("gemini-pro-orchestrator".into()),
        "orchestrator (argon)" => return Some("gemini-pro-orchestrator-argon".into()),
        _ => {}
    }
    let (body, effort) = match lower.strip_suffix(')') {
        Some(rest) => {
            let (body, effort) = rest.rsplit_once(" (")?;
            let effort = effort.trim();
            if effort.is_empty() || !effort.bytes().all(|c| c.is_ascii_alphanumeric()) {
                return None;
            }
            (body, Some(effort))
        }
        None => (lower.as_str(), None),
    };
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut slug = match words.as_slice() {
        ["gemini", ver, variant @ ..] if !variant.is_empty() => {
            format!("gemini-{ver}-{}", variant.join("-"))
        }
        ["claude", tier, ver] => format!("claude-{tier}-{}", ver.replace('.', "-")),
        ["gpt-oss", size] => format!("gpt-oss-{size}"),
        _ => return None,
    };
    if let Some(e) = effort {
        slug = format!("{slug}-{e}");
    }
    slug_ok(&slug).then_some(slug)
}

/// Detect the configured Antigravity model from `OMS_MODEL` or antigravity-cli settings.
pub fn detect_antigravity_model() -> Option<String> {
    if let Some(m) = std::env::var("OMS_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return normalize_antigravity_model(&m);
    }
    let path = match std::env::var_os("OMS_ANTIGRAVITY_SETTINGS").filter(|v| !v.is_empty()) {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(std::env::var_os("HOME").filter(|v| !v.is_empty())?)
            .join(".gemini/antigravity-cli/settings.json"),
    };
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 256 * 1024 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let doc: Value = serde_json::from_slice(&bytes).ok()?;
    normalize_antigravity_model(doc.as_object()?.get("model")?.as_str()?)
}

fn resolve_main(
    policy: &routing::Policy,
    capability: &MainCapability,
    model: Option<&str>,
) -> MainResolution {
    let owner = capability.owner;
    if capability.uses_agy_settings() {
        return MainResolution {
            owner: owner.into(),
            model: match model {
                Some(m) => normalize_antigravity_model(m).or_else(|| {
                    Some(m.trim().to_owned())
                        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
                }),
                None => detect_antigravity_model(),
            },
            effort: "native-settings".into(),
        };
    }
    let preset = policy.main.get(owner);
    MainResolution {
        owner: owner.into(),
        model: model
            .map(str::to_owned)
            .or_else(|| preset.map(|p| p.model.clone())),
        effort: preset.map_or_else(|| "medium".into(), |p| p.effort.clone()),
    }
}

fn load_policy(source: &Path) -> Result<routing::Policy> {
    let path = source.join("config/panel-routing.json");
    if path.exists() {
        routing::Policy::load(&path)
    } else {
        routing::Policy::bundled()
    }
}

fn resolve_or_legacy_session(
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<(String, Option<String>)> {
    let canon = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let repo_str = canon.to_string_lossy().into_owned();
    let candidates = session_candidates(&canon);
    for name in &candidates {
        let owner = session_owner(services, repo, name, stop);
        if owner.is_none() || owner.as_deref() == Some(repo_str.as_str()) {
            return Ok((name.clone(), owner));
        }
    }
    if let Ok(panes) = discover_panel_panes(&canon, services, stop) {
        let found: BTreeSet<String> = panes
            .into_iter()
            .map(|p| p.session)
            .filter(|s| !candidates.contains(s))
            .collect();
        if found.len() == 1 {
            let legacy = found.into_iter().next().unwrap();
            if session_owner(services, repo, &legacy, stop).as_deref() == Some(repo_str.as_str()) {
                return Ok((legacy, Some(repo_str)));
            }
        }
    }
    let last = candidates.last().cloned().unwrap();
    let owner = session_owner(services, repo, &last, stop);
    Ok((last, owner))
}

fn session_candidates(repo: &Path) -> Vec<String> {
    let raw_name = repo
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_lowercase();
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in raw_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "project" } else { slug };
    let digest = oms_panel_core::digest(repo.to_string_lossy().as_bytes());
    vec![
        format!("oms-{slug}"),
        format!("oms-{slug}-{}", &digest[..6]),
    ]
}

fn session_owner(
    services: &dyn Services,
    repo: &Path,
    session: &str,
    stop: &AtomicBool,
) -> Option<String> {
    let has = services
        .execute(
            &ExecutionRequest::new(
                crate::commands::strings(&["tmux", "has-session", "-t", &format!("={session}")]),
                repo,
            ),
            stop,
        )
        .ok()?;
    if has.code != 0 {
        return None;
    }
    Some(
        tmux_quiet(
            services,
            repo,
            &[
                "show-options",
                "-v",
                "-t",
                &format!("={session}:"),
                "@oms_panel_repo",
            ],
            stop,
        )
        .unwrap_or_default(),
    )
}

fn recorded_room(
    services: &dyn Services,
    repo: &Path,
    session: &str,
    stop: &AtomicBool,
) -> Option<String> {
    let val = tmux_quiet(
        services,
        repo,
        &[
            "show-environment",
            "-t",
            &format!("={session}"),
            "OMS_ROOM_ID",
        ],
        stop,
    )?;
    let (_, rest) = val.split_once('=')?;
    let trimmed = rest.trim();
    if crate::identifier(trimmed).is_ok() {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn check_reattach_room(
    repo: &Path,
    base: &PanelPane,
    requested: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let Some(requested) = requested else {
        return Ok(());
    };
    crate::identifier(requested)?;
    let bound = tmux_quiet(
        services,
        repo,
        &[
            "show-option",
            "-w",
            "-v",
            "-t",
            &base.pane,
            "@oms_panel_room",
        ],
        stop,
    )
    .ok_or_else(|| Error::new("existing Main room is unavailable; existing sessions preserved"))?;
    require(
        bound == requested,
        "existing Main belongs to a different room; existing sessions preserved",
    )?;
    let room = crate::commands::query(
        services,
        NativeCommand::Room,
        repo,
        &["show", "--id", requested, "--exact", "--json"],
        stop,
    )?;
    require(
        room["id"] == requested && room["closed"] == false,
        "selected Main room is unavailable or closed; existing sessions preserved",
    )
}

fn panel_room(
    repo: &Path,
    session: &str,
    options: &Options,
    lock: &oms_panel_core::Transaction<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let selected =
        crate::commands::room(options).or_else(|| recorded_room(services, repo, session, stop));
    lock.check()?;
    let ident = ensure_room(repo, selected.as_deref(), services, stop)?;
    lock.check()?;
    let _ = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&[
            "set-environment",
            "-t",
            &format!("={session}"),
            "OMS_ROOM_ID",
            &ident,
        ]),
        stop,
    );
    Ok(ident)
}

fn ensure_room(
    repo: &Path,
    ident: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    if let Some(id) = ident {
        let state = crate::commands::query(
            services,
            NativeCommand::Room,
            repo,
            &["show", "--id", id, "--json"],
            stop,
        )?;
        require(state["closed"] != true, "selected room is closed")?;
        return Ok(text(&state, "id").into());
    }
    let created = crate::commands::query(
        services,
        NativeCommand::Room,
        repo,
        &["new", "--topic", "Shared work", "--json"],
        stop,
    )?;
    let id = text(&created, "id");
    require(!id.is_empty(), "room creation unavailable")?;
    Ok(id.into())
}

fn tmux_host_refs(repo: &Path, services: &dyn Services, stop: &AtomicBool) -> Vec<String> {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return vec![];
    };
    if !crate::commands::valid_target(&pane, '%') {
        return vec![];
    }
    let Some(raw) = tmux_quiet(
        services,
        repo,
        &["display-message", "-p", "#{pid}\t#{socket_path}"],
        stop,
    ) else {
        return vec![];
    };
    let fields: Vec<&str> = raw.trim().split('\t').collect();
    let [pid_raw, socket_raw] = fields.as_slice() else {
        return vec![];
    };
    let Ok(pid) = pid_raw.parse::<u32>() else {
        return vec![];
    };
    let Ok(server) = terminals::process(pid) else {
        return vec![];
    };
    let Ok(canon_socket) = std::fs::canonicalize(socket_raw) else {
        return vec![];
    };
    let socket_digest = oms_panel_core::digest(canon_socket.as_os_str().as_encoded_bytes());
    vec![
        format!("panel_pane_id={pane}"),
        format!("panel_tmux_server_pid={}", server.pid),
        format!("panel_tmux_server_identity={}", server.birth),
        format!("panel_socket_digest={socket_digest}"),
    ]
}

fn release_dead_mains(
    repo: &Path,
    start_lock: &oms_panel_core::Transaction<'_>,
    sess_lock: &oms_panel_core::Transaction<'_>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let raw = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let classified = release::classify(repo, rows(&raw), services);
    for item in rows(&classified["releasable"]) {
        if let Some(id) = item["attempt"].as_str()
            && release::release_locked(repo, id, start_lock, sess_lock, services, stop).is_ok()
        {
            eprintln!("oms panel: released crashed main {id}");
        }
    }
    Ok(())
}

fn pending_window_id(repo: &Path, services: &dyn Services, stop: &AtomicBool) -> Option<String> {
    if !managed_session() {
        return None;
    }
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|p| crate::commands::valid_target(p, '%'))?;
    tmux_quiet(
        services,
        repo,
        &["display-message", "-p", "-t", &pane, "#{window_id}"],
        stop,
    )
    .filter(|w| crate::commands::valid_target(w, '@'))
}

fn check_legacy_main(
    repo: &Path,
    session: &str,
    pending_window: Option<&str>,
    locks: Option<(
        &oms_panel_core::Transaction<'_>,
        &oms_panel_core::Transaction<'_>,
    )>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    if let Some((start_lock, sess_lock)) = locks {
        release_dead_mains(repo, start_lock, sess_lock, services, stop)?;
    }
    let raw = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(
        !rows(&raw)
            .iter()
            .any(|row| row["tool"] == "panel-main" && row["terminal"] != true),
        "another main is live or unverified; finish it before starting another",
    )?;
    if session_owner(services, repo, session, stop).is_some() {
        require(
            live_mains(repo, session, None, pending_window, services, stop)? < MAX_LIVE_MAINS,
            "another main is live or unverified; finish it before starting another",
        )?;
    }
    Ok(())
}

fn live_mains(
    repo: &Path,
    session: &str,
    room_id: Option<&str>,
    pending_window: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<usize> {
    // Keep both ends nonempty so tmux output trimming preserves unset option columns.
    let raw = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&[
            "list-windows",
            "-t",
            &format!("={session}"),
            "-F",
            "#{window_id}\t#{@oms_panel_owner}\t#{@oms_panel_room}\t#{@oms_panel_room_participant}\t#{window_name}\t#{@oms_panel_harness}\t#{@oms_provider_session}\t.",
        ]),
        stop,
    )?;
    let mut windows = Vec::new();
    for line in raw.replace('\r', "").lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [window, owner, room, member, name, harness, provider, "."] = fields.as_slice() else {
            return Err(Error::new(
                "main window status unavailable; existing sessions preserved",
            ));
        };
        require(
            !name.trim().is_empty() && crate::commands::valid_target(window, '@'),
            "main window status unavailable; existing sessions preserved",
        )?;
        if crate::orchestrator::PROVIDERS.contains(provider)
            && owner.is_empty()
            && member.is_empty()
            && harness.is_empty()
        {
            continue;
        }
        let native = !member.is_empty()
            || !harness.is_empty()
            || (!owner.is_empty() && *name != "control")
            || main_capability(name).is_some();
        if !native {
            if *name != "control" {
                return Err(Error::new(
                    "main window ownership is unverified; existing sessions preserved",
                ));
            }
            continue;
        }
        if pending_window.is_some_and(|pw| pw == *window && member.is_empty()) {
            continue;
        }
        windows.push((room.to_string(), member.to_string()));
    }

    let mut rooms = BTreeSet::new();
    if let Some(id) = room_id
        && !id.is_empty()
    {
        rooms.insert(id.to_owned());
    }
    for (r, _) in &windows {
        if !r.is_empty() {
            rooms.insert(r.clone());
        }
    }
    let all_attempts = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;

    let mut live: BTreeSet<(String, String)> = BTreeSet::new();
    let mut terminal: BTreeSet<(String, String)> = BTreeSet::new();
    for ident in &rooms {
        let state = crate::commands::query(
            services,
            NativeCommand::Room,
            repo,
            &["show", "--id", ident, "--json"],
            stop,
        )?;
        let joined: BTreeSet<&str> = rows(&state["participants"])
            .iter()
            .filter(|p| p["joined"] == true && p["role"] == "main")
            .filter_map(|p| p["participant"].as_str())
            .collect();
        for row in rows(&all_attempts) {
            if row["tool"] != "panel-main" || row["refs"]["panel_room_id"] != ident.as_str() {
                continue;
            }
            let who = oms_panel_core::state::main_participant(row).to_owned();
            if row["terminal"] == true {
                terminal.insert((ident.clone(), who));
            } else if joined.contains(who.as_str()) {
                live.insert((ident.clone(), who));
            }
        }
    }
    let mut unnamed = 0usize;
    for (r, member) in windows {
        if member.is_empty() {
            unnamed += 1;
        } else if !terminal.contains(&(r.clone(), member.clone())) {
            live.insert((r, member));
        }
    }
    Ok(live.len() + unnamed)
}

fn native_resume_binding(
    provider: &str,
    repo: &Path,
    resume: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<BTreeMap<String, String>> {
    let digest = &oms_panel_core::digest(resume.as_bytes())[..32];
    let raw = crate::commands::query(
        services,
        NativeCommand::Room,
        repo,
        &["list", "--json"],
        stop,
    )?;
    require(
        raw["incomplete"] == false,
        "native resume ownership discovery is incomplete; inspect existing sessions",
    )?;
    let mut bound = BTreeMap::new();
    let rooms = raw["rooms"]
        .as_array()
        .ok_or_else(|| Error::new("native resume room enumeration is unavailable"))?;
    // List exposes summaries; current participant bindings belong to show.
    // Missing discovery evidence must not become an unbound-session claim.
    for summary in rooms {
        if summary["successor"].as_str().is_some() {
            continue;
        }
        let id = summary["id"]
            .as_str()
            .ok_or_else(|| Error::new("native resume room identity is unavailable"))?;
        let state = crate::commands::query(
            services,
            NativeCommand::Room,
            repo,
            &["show", "--id", id, "--json"],
            stop,
        )?;
        let participants = state["participants"]
            .as_array()
            .ok_or_else(|| Error::new("native resume participant bindings are unavailable"))?;
        let current_id = state["id"]
            .as_str()
            .ok_or_else(|| Error::new("native resume current room identity is unavailable"))?;
        for enrolled in participants {
            if enrolled["provider"] == provider
                && enrolled["joined"] == true
                && enrolled["consumer"] == digest
            {
                let participant = enrolled["participant"].as_str().ok_or_else(|| {
                    Error::new("native resume participant identity is unavailable")
                })?;
                require(
                    bound
                        .insert(current_id.to_owned(), participant.to_owned())
                        .is_none(),
                    "native session has duplicate room participants; resume refused",
                )?;
            }
        }
    }
    Ok(bound)
}

fn tmux_quiet(
    services: &dyn Services,
    repo: &Path,
    args: &[&str],
    stop: &AtomicBool,
) -> Option<String> {
    let mut argv = vec![OsString::from("tmux")];
    argv.extend(crate::commands::strings(args));
    let out = services
        .execute(&ExecutionRequest::new(argv, repo), stop)
        .ok()?;
    if out.code == 0 {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        None
    }
}

fn managed_session() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
        && std::env::var("OMS_PANEL_SESSION")
            .ok()
            .is_some_and(|s| valid_panel_session(&s))
}

fn valid_panel_session(name: &str) -> bool {
    name.starts_with("oms-")
        && name.len() <= 52
        && name.len() > 4
        && name[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn which_exists(binary: &str) -> bool {
    let candidate = Path::new(binary);
    if candidate.is_absolute() {
        return is_exec(candidate);
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| is_exec(&dir.join(candidate))))
}

fn is_exec(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
        .unwrap_or(false)
}

fn safe_label(raw: Option<&str>) -> String {
    match raw {
        Some(v) if !v.trim().is_empty() && !v.contains('/') && !v.contains('\\') => {
            crate::render::clean(v, 120)
        }
        _ => "Native task not recorded".into(),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    let hex = oms_panel_core::digest(format!("{nanos}:{pid}:claude-main").as_bytes());
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..15],
        &hex[15..18],
        &hex[18..30]
    )
}

/// tmux `status-right` for a managed session: the live OMS HUD, refreshed by
/// tmux's own `status-interval`, with plain `oms` when the HUD cannot run.
/// tmux expands `#` and `%` (and balances parentheses) inside `#( )`, so a repo
/// path containing them is not interpolated at all.
fn hud_status_right(prefix: &[String], repo: &Path) -> String {
    const FALLBACK: &str = "oms";
    let repo = repo.to_string_lossy();
    if repo.contains(['#', '%', '(', ')', '\n', '\r'])
        || prefix
            .iter()
            .any(|p| p.contains(['#', '%', '(', ')', '\n', '\r']))
    {
        return FALLBACK.into();
    }
    let program = prefix
        .iter()
        .map(|p| crate::commands::shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    let repo = crate::commands::shell_quote(&repo);
    format!("#({program} hud --tmux --repo {repo} 2>/dev/null || printf {FALLBACK})")
}

#[cfg(test)]
pub(crate) fn test_entry_prefix(source: &Path) -> Result<Vec<String>> {
    entry_prefix(source)
}

#[cfg(test)]
pub(crate) fn test_entry_matches(repo: &Path, prefix: &[String]) -> bool {
    let command = prefix
        .iter()
        .map(|p| crate::commands::shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    let repo_arg = crate::commands::shell_quote(&repo.to_string_lossy());
    let command = format!("{command} panel --repo {repo_arg}");
    match_panel_command(&command, repo) == Some(false)
        && match_panel_command(&format!("{command} --watch"), repo) == Some(true)
}

#[cfg(test)]
mod hud_status {
    use super::hud_status_right;
    use std::path::Path;

    #[test]
    fn hud_status_runs_the_entry_with_a_plain_fallback() {
        let prefix = vec!["/opt/oms bin/oms".to_string()];
        assert_eq!(
            hud_status_right(&prefix, Path::new("/work/it's")),
            "#('/opt/oms bin/oms' hud --tmux --repo '/work/it'\\''s' 2>/dev/null || printf oms)"
        );
    }

    #[test]
    fn hud_status_never_interpolates_tmux_metacharacters() {
        for repo in ["/w/a#b", "/w/50%", "/w/(x)", "/w/a\nb"] {
            assert_eq!(hud_status_right(&["oms".into()], Path::new(repo)), "oms");
        }
        assert_eq!(
            hud_status_right(&["/o/#oms".into()], Path::new("/w")),
            "oms"
        );
    }
}

#[cfg(test)]
pub(crate) mod native_lifecycle_contract {
    use super::{event_id, heartbeat_attempt, transition_attempt};
    use oms_panel_core::{
        NativeCommand, Result, Services, Value,
        services::{CommandOutput, ExecutionRequest, PeerExecutor},
    };
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempRepo(PathBuf);
    impl TempRepo {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "oms-panel-real-parsers-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
            let p = p.canonicalize().unwrap();
            let init = std::process::Command::new("git")
                .args(["-C", p.to_str().unwrap(), "init", "-q"])
                .status()
                .unwrap();
            assert!(init.success());
            Self(p)
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn main_swap_exact_room_native_exit_and_same_pane_with_mock_host() {
        use super::{
            SWAP_COMPOSER, SWAP_PANE, SwapStage, perform_main_swap, swap_record, terminals,
        };
        use oms_panel_core::{Error, Store, release::MainSwapRequest};
        use serde_json::json;
        use std::cell::{Cell, RefCell};
        use std::process::{Child, Command};
        use std::time::Instant;

        const CHILD_ENV: &str = "OMS_PANEL_MAIN_SWAP_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let home = TempRepo::new();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.env_clear();
            for key in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let output = command
                .args([
                    "--exact",
                    "native_panel::native_lifecycle_contract::main_swap_exact_room_native_exit_and_same_pane_with_mock_host",
                    "--nocapture",
                ])
                .env("HOME", &home.0)
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(stdout.contains("1 passed;"), "{stdout}");
            return;
        }

        const OLD: &str = "att_11111111111111111111111111111111";
        const NEW: &str = "att_22222222222222222222222222222222";
        const MEMBER: &str = "att_33333333333333333333333333333333";
        const ROOM: &str = "room-swap-fixture";
        const MODEL: &str = "fixture-agy-model";
        static STOP: AtomicBool = AtomicBool::new(false);

        struct Mock {
            old_wrapper: RefCell<Child>,
            old_native: RefCell<Child>,
            new_wrapper: RefCell<Child>,
            new_native: RefCell<Child>,
            old: Value,
            new: Value,
            draft: Cell<bool>,
            dead: Cell<bool>,
            committed: Cell<bool>,
            respawned: Cell<bool>,
            sent: Cell<usize>,
            launched: Cell<usize>,
            mode: &'static str,
            room_successor: Cell<bool>,
            preflights: Cell<usize>,
            pastes: Cell<usize>,
            remain: Cell<&'static str>,
        }
        fn stop_child(child: &mut Child) {
            let _ = child.kill();
            child.wait().unwrap();
        }
        impl Drop for Mock {
            fn drop(&mut self) {
                for child in [
                    &mut self.old_wrapper,
                    &mut self.old_native,
                    &mut self.new_wrapper,
                    &mut self.new_native,
                ] {
                    stop_child(child.get_mut());
                }
            }
        }
        fn out(text: impl Into<Vec<u8>>) -> CommandOutput {
            CommandOutput {
                stdout: text.into(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            }
        }
        impl Mock {
            fn row(&self, repo: &Path) -> String {
                let (pid, dead, attempt, owner, harness) = if self.respawned.get() {
                    (self.new_wrapper.borrow().id(), 0, NEW, "antigravity", "agy")
                } else {
                    (
                        self.old_wrapper.borrow().id(),
                        u8::from(self.dead.get()),
                        OLD,
                        "codex",
                        "codex",
                    )
                };
                format!(
                    "oms-fixture\t$1\t@1\t%1\t{pid}\t{dead}\t\t%1\t{attempt}\t{MEMBER}\t{ROOM}\t{owner}\t{harness}\t{}\t{}\t.",
                    repo.display(),
                    repo.display()
                )
            }
        }
        impl PeerExecutor for Mock {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                let args: Vec<_> = request.argv.iter().map(|a| a.to_str().unwrap()).collect();
                let repo = &request.cwd;
                match args.as_slice() {
                    ["tmux", "show-options", "-pqv", "-t", "%1", "remain-on-exit"] => {
                        Ok(out(self.remain.get()))
                    }
                    ["tmux", "list-panes", "-a", "-F", SWAP_PANE]
                    | ["tmux", "display-message", "-p", "-t", "%1", SWAP_PANE] => {
                        Ok(out(self.row(repo)))
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
                    ] => Ok(out(format!(
                        "$1\t{}\tfixture.sock\t.\n{}\n",
                        std::process::id(),
                        repo.display()
                    ))),
                    [
                        "tmux",
                        "display-message",
                        "-p",
                        "-t",
                        "%1",
                        SWAP_COMPOSER,
                        ";",
                        "capture-pane",
                        "-p",
                        "-e",
                        "-S",
                        "0",
                        "-t",
                        "%1",
                    ] => {
                        let (cursor_x, cursor_y, input) = match (self.mode, self.draft.get()) {
                            ("busy", false) => (2, 1, "• Working (2s • esc to interrupt)\n›"),
                            ("attachment", false) => (2, 0, "›\n[Image attached]"),
                            (_, true) => (7, 0, "› /exit"),
                            _ => (2, 0, "›"),
                        };
                        Ok(out(format!(
                            "0\t0\t{cursor_x}\t{cursor_y}\tcodex\t.\n{input}\n\n  GPT-6.2-Sol high · Context 9% left · weekly 13% left · main · ~/fixture · Main [default]\n← for agents · ? for shortcuts"
                        )))
                    }
                    ["tmux", "if-shell", "-F", "-t", "%1", fence, command, _] => {
                        assert!(fence.contains(MEMBER) && fence.contains(ROOM));
                        let expected = if self.respawned.get() { NEW } else { OLD };
                        assert!(fence.contains(expected));
                        if command.contains("remain-on-exit") {
                            let value = if fence.contains("#{==:#{remain-on-exit},on}") {
                                assert!(self.respawned.get() || !self.dead.get());
                                if command.contains("'-pu'") {
                                    ""
                                } else {
                                    assert!(command.contains("'failed'"));
                                    "failed"
                                }
                            } else {
                                assert!(!self.dead.get() && command.contains("'on'"));
                                "on"
                            };
                            self.remain.set(value);
                        } else if command.contains("'/exit'") {
                            self.pastes.set(self.pastes.get() + 1);
                            if self.mode == "paste-fence" {
                                return Ok(out("OMS_MAIN_SWAP_FENCE_CHANGED"));
                            }
                            assert!(!self.draft.replace(true));
                            if self.mode == "paste-unknown" {
                                return Err(Error::new("paste outcome unknown"));
                            }
                        } else if command.contains("'Enter'") {
                            assert!(self.draft.get());
                            self.sent.set(self.sent.get() + 1);
                            if self.mode == "fence" {
                                return Ok(out("OMS_MAIN_SWAP_FENCE_CHANGED"));
                            }
                            if self.mode == "uncertain" {
                                return Err(Error::new("send outcome unknown"));
                            }
                            stop_child(&mut self.old_wrapper.borrow_mut());
                            stop_child(&mut self.old_native.borrow_mut());
                            self.dead.set(true);
                        } else if command.contains("'respawn-pane'") {
                            assert!(
                                self.committed.get()
                                    && self.dead.get()
                                    && !command.contains("'-k'")
                            );
                            self.launched.set(self.launched.get() + 1);
                            if self.mode == "startup" {
                                return Err(Error::new("native startup uncertain"));
                            }
                            self.respawned.set(true);
                        } else {
                            panic!("unexpected mutation {command}");
                        }
                        Ok(out(""))
                    }
                    _ => panic!("unexpected fake tmux {args:?}"),
                }
            }
        }
        impl Services for Mock {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let value = match command {
                    NativeCommand::Room => {
                        let provider = if self.committed.get() {
                            "antigravity"
                        } else {
                            "codex"
                        };
                        let swaps = self.committed.get().then(|| json!({"attempt":OLD,"participant":MEMBER,"to":"antigravity","model":MODEL,"harness":"agy"}));
                        json!({"id":ROOM,"closed":false,"successor":self.room_successor.get().then_some("room-successor"),
                            "participants":[{"participant":MEMBER,"provider":provider,"joined":true,"role":"main"}],"swaps":swaps.into_iter().collect::<Vec<_>>()})
                    }
                    NativeCommand::Events => {
                        let mut row = if args[2] == NEW {
                            self.new.clone()
                        } else {
                            self.old.clone()
                        };
                        if args[2] == OLD && self.dead.get() {
                            let receipt = swap_record(&Store::open(repo)?, OLD)?.unwrap();
                            assert!(receipt.stage == SwapStage::ExitSent);
                            row["terminal"] = true.into();
                            row["state"] = "abandoned".into();
                            row["refs"]["panel_native_exit"] = "confirmed".into();
                            row["refs"]["panel_native_exit_code"] =
                                if self.mode == "crash" { "130" } else { "0" }.into();
                            row["reason_code"] = if self.mode == "unacked" {
                                "native_exit"
                            } else {
                                "main_swapped"
                            }
                            .into();
                        }
                        row
                    }
                    _ => panic!("unexpected fake command {command:?}"),
                };
                Ok(out(serde_json::to_vec(&value).unwrap()))
            }
            fn check_main_swap_source(&self, _: &Path, _: &Value, _: &AtomicBool) -> Result<()> {
                let check = self.preflights.get() + 1;
                self.preflights.set(check);
                crate::require(self.mode != "team" || check != 2, "active Team contracts")
            }
            fn commit_main_swap(
                &self,
                _: &Path,
                room: &str,
                member: &str,
                request: &MainSwapRequest,
                _: Instant,
                _: &AtomicBool,
            ) -> Result<Value> {
                assert!(self.dead.get() && !self.committed.replace(true));
                assert_eq!(
                    (room, member, request.attempt_id.as_str()),
                    (ROOM, MEMBER, OLD)
                );
                Ok(json!({"room":room,"participant":member,"attempt":request.attempt_id}))
            }
        }
        let child = || Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let lifecycle = |wrapper: &Child, native: &Child, attempt, provider, harness| {
            json!({"tool":"panel-main","attempt_id":attempt,"provider":provider,"state":"working","terminal":false,
                "refs":{"panel_room_id":ROOM,"panel_room_participant":MEMBER,"panel_harness":harness,"panel_model":MODEL,
                    (super::MAIN_PERMISSIONS_REF):"native", "panel_host":"tmux", "panel_wrapper_pid":wrapper.id().to_string(),
                    "panel_wrapper_identity":terminals::process(wrapper.id()).unwrap().birth,
                    "panel_native_pid":native.id().to_string(),"panel_native_identity":terminals::process(native.id()).unwrap().birth}})
        };
        for mode in [
            "success",
            "fence",
            "uncertain",
            "paste-fence",
            "paste-unknown",
            "unacked",
            "crash",
            "startup",
            "busy",
            "attachment",
            "team",
        ] {
            let repo = TempRepo::new();
            let old_wrapper = child();
            let old_native = child();
            let new_wrapper = child();
            let new_native = child();
            let old = lifecycle(&old_wrapper, &old_native, OLD, "codex", "codex");
            let new = lifecycle(&new_wrapper, &new_native, NEW, "antigravity", "agy");
            let mock = Mock {
                old_wrapper: old_wrapper.into(),
                old_native: old_native.into(),
                new_wrapper: new_wrapper.into(),
                new_native: new_native.into(),
                old,
                new,
                draft: false.into(),
                dead: false.into(),
                committed: false.into(),
                respawned: false.into(),
                sent: 0.into(),
                launched: 0.into(),
                mode,
                room_successor: false.into(),
                preflights: 0.into(),
                pastes: 0.into(),
                remain: if matches!(mode, "success" | "paste-fence") {
                    "failed"
                } else {
                    ""
                }
                .into(),
            };
            let target = MainSwapRequest {
                attempt_id: OLD.into(),
                provider: "antigravity".into(),
                harness: "agy".into(),
                model: MODEL.into(),
            };
            let result = perform_main_swap(&repo.0, &target, &mock.old, &mock, &STOP, |record| {
                assert_eq!(record.permissions, "native");
                assert!(record.handoff.len() <= 3000 && record.handoff.contains(ROOM));
                Ok("exec /approved/test-command".into())
            });
            if matches!(mode, "busy" | "attachment") {
                assert!(result.unwrap_err().message.contains("empty idle Codex"));
                assert_eq!(mock.sent.get(), 0);
                assert_eq!(mock.launched.get(), 0);
                assert!(
                    swap_record(&Store::open(&repo.0).unwrap(), OLD)
                        .unwrap()
                        .is_none()
                );
                continue;
            }
            if mode == "team" {
                assert!(result.unwrap_err().message.contains("active Team"));
                assert_eq!(mock.preflights.get(), 2);
                assert_eq!(mock.sent.get(), 0);
                assert_eq!(mock.launched.get(), 0);
                assert!(!mock.dead.get());
                assert_eq!(mock.remain.get(), "");
                assert!(
                    swap_record(&Store::open(&repo.0).unwrap(), OLD)
                        .unwrap()
                        .is_some_and(|record| record.stage == SwapStage::DraftConfirmed)
                );
                let retried = perform_main_swap(&repo.0, &target, &mock.old, &mock, &STOP, |_| {
                    Ok("exec /approved/test-command".into())
                })
                .unwrap();
                assert_eq!(retried["attempt"], NEW);
                assert_eq!(retried["pane_policy_restored"], true);
                assert_eq!(mock.pastes.get(), 1);
                assert_eq!(mock.sent.get(), 1);
                assert_eq!(mock.launched.get(), 1);
                assert_eq!(mock.remain.get(), "");
                continue;
            }
            if matches!(mode, "uncertain" | "paste-unknown") {
                let repeated = perform_main_swap(&repo.0, &target, &mock.old, &mock, &STOP, |_| {
                    panic!("uncertain actions must refuse before admission")
                })
                .unwrap_err();
                assert!(repeated.message.contains("unresolved recovery evidence"));
                assert_eq!(mock.pastes.get(), 1);
            }
            assert_eq!(
                mock.sent.get(),
                usize::from(!mode.starts_with("paste-")),
                "{mode}"
            );
            assert_eq!(
                mock.launched.get(),
                usize::from(matches!(mode, "success" | "startup")),
                "{mode}"
            );
            let receipt = swap_record(&Store::open(&repo.0).unwrap(), OLD)
                .unwrap()
                .unwrap();
            match mode {
                "success" => {
                    let output = result.unwrap();
                    assert_eq!(output["participant"], MEMBER);
                    assert_eq!(output["attempt"], NEW);
                    assert_eq!(output["pane_policy_restored"], true);
                    assert_eq!(mock.remain.get(), "failed");
                    assert!(receipt.stage == SwapStage::RoomCommitted);
                    let pid = mock.new_wrapper.borrow().id();
                    super::prove_replacement_custody(&repo.0, &receipt, NEW, pid, &mock, &STOP)
                        .unwrap();
                    assert!(
                        super::prove_replacement_custody(
                            &repo.0,
                            &receipt,
                            NEW,
                            mock.old_wrapper.borrow().id(),
                            &mock,
                            &STOP,
                        )
                        .unwrap_err()
                        .message
                        .contains("pane changed")
                    );
                    mock.room_successor.set(true);
                    assert!(super::prove_replacement_custody(
                        &repo.0, &receipt, NEW, pid, &mock, &STOP,
                    ).unwrap_err().message.contains("Room"));
                }
                "startup" => {
                    assert!(result.is_err() && receipt.stage == SwapStage::RoomCommitted);
                }
                "unacked" | "crash" => {
                    assert!(result.is_err() && receipt.stage == SwapStage::ExitSent)
                }
                mode => {
                    let (stage, policy) = match mode {
                        "fence" => (SwapStage::DraftConfirmed, ""),
                        "uncertain" => (SwapStage::SubmitPending, "on"),
                        "paste-fence" => (SwapStage::BeforeDraft, "failed"),
                        "paste-unknown" => (SwapStage::PastePending, "on"),
                        _ => unreachable!(),
                    };
                    assert!(result.is_err() && !mock.dead.get() && receipt.stage == stage);
                    assert_eq!(mock.remain.get(), policy);
                }
            }
        }
    }

    #[test]
    fn startup_selects_the_explicit_repo_and_preserves_the_bound_room_repo() {
        const CHILD: &str = "OMS_PANEL_TEST_STARTUP_REPO";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_panel::native_lifecycle_contract::startup_selects_the_explicit_repo_and_preserves_the_bound_room_repo",
                ])
                .env(CHILD, "1")
                .env_remove("OMS_ROOM_REPO")
                .env_remove("OMS_STATE_REPO")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let repo = TempRepo::new();
        let other = TempRepo::new();
        let options = crate::cli::Options::parse(&crate::commands::strings(&[
            "--repo",
            repo.0.to_str().unwrap(),
            "--room",
            "room-fixture",
        ]))
        .unwrap();
        assert!(options.selects_main());
        for cwd in [repo.0.parent().unwrap(), other.0.as_path()] {
            let (state, work) =
                crate::commands::resolve_repositories(&options, cwd, Some(&repo.0), true).unwrap();
            assert_eq!(state.as_deref(), Some(repo.0.as_path()));
            assert_eq!(work.as_deref(), Some(repo.0.as_path()));
            assert!(
                crate::commands::resolve_repositories(&options, cwd, Some(&other.0), true)
                    .unwrap_err()
                    .message
                    .contains("conflicts with OMS_ROOM_REPO")
            );
        }
        assert!(!repo.0.join(".oms").exists());
        assert!(!other.0.join(".oms").exists());
    }

    pub(crate) struct RealCliParsers;
    struct RoomOperations {
        repo: PathBuf,
        bytes: Vec<u8>,
        attempts: Vec<oms_command_room::Attempt>,
        _guard: oms_command_operations::lock::FileLock,
    }
    impl oms_command_room::OperationsRead for RoomOperations {
        fn attempts(&self) -> &[oms_command_room::Attempt] {
            &self.attempts
        }
        fn check(&self) -> oms_command_room::Result<()> {
            if fs::read(oms_command_operations::event_path(&self.repo)).unwrap_or_default()
                != self.bytes
            {
                return Err(oms_command_room::Error::new("fixture operations changed"));
            }
            Ok(())
        }
    }
    impl oms_command_room::Services for RealCliParsers {
        fn state_repository(&self, path: &Path) -> oms_command_room::Result<PathBuf> {
            oms_command_room::Services::state_repository(&oms_command_room::LocalServices, path)
        }
        fn canonical_provider(&self, provider: &str) -> oms_command_room::Result<String> {
            oms_command_room::Services::canonical_provider(
                &oms_command_room::LocalServices,
                provider,
            )
        }
        fn operations<'a>(
            &'a self,
            repo: &Path,
        ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>> {
            let error = oms_command_room::Error::new;
            let guard = oms_command_operations::lock::FileLock::acquire(
                &oms_command_operations::event_path(repo),
                &AtomicBool::new(false),
            )
            .map_err(error)?;
            let (_, projection) = oms_command_operations::load_projection(repo).map_err(error)?;
            let attempts = projection
                .iter()
                .map(|row| {
                    let raw = row.json(false).map_err(error)?;
                    let row = oms_command_room::value::parse(&raw, oms_command_room::MAX_ROW)?;
                    oms_command_room::Attempt::from_projection_row(&row)
                })
                .collect::<oms_command_room::Result<Vec<_>>>()?;
            Ok(Box::new(RoomOperations {
                repo: repo.to_path_buf(),
                bytes: fs::read(oms_command_operations::event_path(repo)).unwrap_or_default(),
                attempts,
                _guard: guard,
            }))
        }
    }
    impl PeerExecutor for RealCliParsers {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            Err(oms_panel_core::Error::new("unexpected subprocess"))
        }
    }
    impl Services for RealCliParsers {
        fn command(
            &self,
            command: NativeCommand,
            repo: &Path,
            args: &[OsString],
            stop: &AtomicBool,
        ) -> Result<CommandOutput> {
            let (code, stdout, stderr) = match command {
                NativeCommand::Events => oms_command_operations::run(
                    oms_command_operations::App::Events,
                    args,
                    repo,
                    stop,
                ),
                NativeCommand::Room => oms_command_room::run_with_services(args, repo, stop, self),
                _ => return Err(oms_panel_core::Error::new("unsupported test command")),
            };
            if code != 0 {
                return Err(oms_panel_core::Error::new(format!(
                    "command exit {code}: {}",
                    String::from_utf8_lossy(&stderr)
                )));
            }
            Ok(CommandOutput {
                code,
                stdout,
                stderr,
                cleanup_complete: true,
                output_complete: true,
                limited: false,
            })
        }
    }

    struct AccountMainTmux<'a> {
        room: &'a str,
        provider: &'a str,
        mutation: std::cell::RefCell<Vec<String>>,
        tags: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
        samples: std::cell::Cell<usize>,
        change_sample: usize,
    }

    impl PeerExecutor for AccountMainTmux<'_> {
        fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            let args: Vec<_> = request
                .argv
                .iter()
                .map(|word| word.to_str().unwrap())
                .collect();
            let tags = self.tags.borrow();
            let tag = |key| tags.get(key).map(String::as_str).unwrap_or("");
            let pid = std::process::id();
            let stdout = match args.as_slice() {
                ["tmux", "display-message", "-p", "-t", "%1", format]
                    if format.contains("pane_pid") =>
                {
                    let sample = self.samples.get() + 1;
                    self.samples.set(sample);
                    let window = if sample == self.change_sample {
                        let start = std::time::Instant::now();
                        while !request.cwd.join(".oms/main-account-child").exists() {
                            if start.elapsed() >= std::time::Duration::from_secs(2) {
                                return Err(oms_panel_core::Error::new(
                                    "fixture child did not start",
                                ));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        "@9"
                    } else {
                        "@1"
                    };
                    format!(
                        "oms-fixture\t$1\t{window}\t%1\t{pid}\t0\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t.",
                        self.provider,
                        self.provider,
                        self.room,
                        tag("@oms_panel_main_attempt"),
                        tag("@oms_panel_room_participant"),
                        tag("@oms_panel_native_pane"),
                        tag(super::MAIN_ACCOUNT_READY),
                    )
                }
                [
                    "tmux",
                    "display-message",
                    "-p",
                    "-t",
                    "$1",
                    "#{session_id}\t#{pid}\t#{socket_path}",
                ] => format!("$1\t{pid}\tfixture-socket"),
                [
                    "tmux",
                    "show-options",
                    "-v",
                    "-t",
                    "$1" | "=oms-fixture:",
                    "@oms_panel_repo",
                ] => request.cwd.to_str().unwrap().into(),
                ["tmux", "has-session", "-t", "=oms-fixture"] => String::new(),
                ["tmux", "list-windows", "-t", "=oms-fixture", "-F", _] => format!(
                    "@1\t{}\t{}\t{}\t{}\t{}\t\t.",
                    self.provider,
                    self.room,
                    tag("@oms_panel_room_participant"),
                    self.provider,
                    self.provider,
                ),
                ["tmux", "set-option", "-w", "-t", "%1", key, value] => {
                    drop(tags);
                    self.tags
                        .borrow_mut()
                        .insert((*key).into(), (*value).into());
                    String::new()
                }
                ["tmux", "list-panes", "-a", "-F", _] => {
                    let repo = crate::commands::shell_quote(request.cwd.to_str().unwrap());
                    format!(
                        "oms-fixture\t@1\t%1\t0\t\toms panel --repo {repo} --launch {} --account first\noms-fixture\t@1\t%2\t0\t\toms panel --repo {repo} --watch",
                        self.provider,
                    )
                }
                [
                    "tmux",
                    action @ ("select-window" | "select-pane"),
                    "-t",
                    "%1",
                ] => {
                    self.mutation.borrow_mut().push((*action).into());
                    String::new()
                }
                _ => {
                    return Err(oms_panel_core::Error::new(
                        "unexpected fixture pane request",
                    ));
                }
            };
            Ok(CommandOutput {
                stdout: stdout.into_bytes(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            })
        }
    }

    impl Services for AccountMainTmux<'_> {
        fn command(
            &self,
            command: NativeCommand,
            repo: &Path,
            args: &[OsString],
            stop: &AtomicBool,
        ) -> Result<CommandOutput> {
            RealCliParsers.command(command, repo, args, stop)
        }
    }

    #[test]
    fn explicit_main_account_managed_launch_and_reuse() {
        use super::{AccountHost, SystemAccountHost, account_binding};
        use account_binding::test_support::{FakeHost, private_dir};
        const NAME: &str = "native_panel::native_lifecycle_contract::explicit_main_account_managed_launch_and_reuse";
        const CASE: &str = "OMS_MAIN_ACCOUNT_TEST_CASE";
        let Ok(case) = std::env::var(CASE) else {
            for case in [
                "codex-first",
                "codex-second",
                "claude-first",
                "claude-second",
                "missing",
                "wrong-room",
                "unfinished",
                "lost-pane",
                "reuse",
                "swapped-reuse",
                "unproved-reuse",
            ] {
                let home = TempRepo::new();
                let repo = TempRepo::new();
                let bin = private_dir(&home.0.join("bin"));
                let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                    .map(|dir| dir.join("git"))
                    .find(|path| path.is_file())
                    .unwrap();
                std::os::unix::fs::symlink(git.canonicalize().unwrap(), bin.join("git")).unwrap();
                let mut providers = serde_json::Map::new();
                for (provider, key) in [("codex", "CODEX_HOME"), ("claude", "CLAUDE_CONFIG_DIR")] {
                    let accounts: Vec<_> = ["first", "second"]
                        .into_iter()
                        .map(|id| {
                            let path =
                                private_dir(&home.0.join(format!("private-{provider}-{id}")));
                            serde_json::json!({"id": id, "home": path})
                        })
                        .collect();
                    providers.insert(provider.into(), serde_json::json!({"accounts": accounts}));
                    let program = bin.join(provider);
                    let mut script = format!(
                        "#!/bin/sh\nif [ \"${key}\" != \"$HOME/private-{provider}-$OMS_MAIN_ACCOUNT_TEST_ALIAS\" ]; then\n  exit 99\nfi\nprintf '%s\\0' \"$@\" > \"$OMS_STATE_REPO/.oms/main-account-argv\"\nif [ \"$OMS_MAIN_ACCOUNT_TEST_CASE\" = lost-pane ]; then\n  printf '%s\\n' \"$$\" > \"$OMS_STATE_REPO/.oms/main-account-child\"\n  exec /bin/sleep 5\nfi\n",
                    );
                    if provider == "claude" {
                        // Hold the synthetic process until the wrapper records this exact Child.
                        script.push_str(
                            r#"
printf '%s\n' "$$" > "$OMS_STATE_REPO/.oms/main-account-child"
i=0
while [ "$i" -lt 500 ]; do
  for file in "$XDG_STATE_HOME"/oh-my-setting/panel-control/*/account-bindings/*.json; do
    [ -f "$file" ] || continue
    IFS= read -r line < "$file"
    case "$line" in *'"claude_main_child"'*) exit 0;; esac
  done
  i=$((i + 1))
  /bin/sleep 0.01
done
exit 98
"#,
                        );
                    }
                    fs::write(&program, script).unwrap();
                    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                }
                if case != "missing" {
                    let registry = private_dir(&home.0.join(".config/oh-my-setting"));
                    fs::write(
                        registry.join("provider-accounts.json"),
                        serde_json::to_vec(
                            &serde_json::json!({"schema": 1, "providers": providers}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
                let alias = if case.ends_with("-second") {
                    "second"
                } else {
                    "first"
                };
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", NAME, "--nocapture"])
                    .env_clear()
                    .env("HOME", &home.0)
                    .env("XDG_STATE_HOME", private_dir(&home.0.join("state")))
                    .env("PATH", &bin)
                    .env("CODEX_HOME", home.0.join("ambient-codex"))
                    .env("CLAUDE_CONFIG_DIR", home.0.join("ambient-claude"))
                    .env("OMS_PANEL_SESSION", "oms-fixture")
                    .env("TMUX", "fixture")
                    .env("TMUX_PANE", "%1")
                    .env(CASE, case)
                    .env("OMS_MAIN_ACCOUNT_TEST_ALIAS", alias)
                    .env("OMS_MAIN_ACCOUNT_TEST_REPO", &repo.0)
                    .output()
                    .unwrap();
                let combine = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    output.status.success() && combine.contains("1 passed;"),
                    "{case}: {combine}",
                );
                assert!(
                    !combine.contains("private-codex-") && !combine.contains("private-claude-")
                );
            }
            return;
        };
        let repo = PathBuf::from(std::env::var_os("OMS_MAIN_ACCOUNT_TEST_REPO").unwrap());
        let alias = std::env::var("OMS_MAIN_ACCOUNT_TEST_ALIAS").unwrap();
        let provider = if case.starts_with("claude") {
            "claude"
        } else {
            "codex"
        };
        let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let policy = super::load_policy(source).unwrap();
        let stop = AtomicBool::new(false);
        let room = super::ensure_room(&repo, None, &RealCliParsers, &stop).unwrap();
        let svc = AccountMainTmux {
            room: &room,
            provider,
            mutation: Default::default(),
            tags: Default::default(),
            samples: Default::default(),
            change_sample: if case == "lost-pane" { 4 } else { 0 },
        };
        let requested_room = if case == "wrong-room" {
            "different-room"
        } else {
            &room
        };
        let parse = |account, room| {
            crate::cli::Options::parse(&crate::commands::strings(&[
                "--launch",
                provider,
                "--host",
                "inline",
                "--account",
                account,
                "--room",
                room,
            ]))
            .unwrap()
        };
        let options = parse(&alias, requested_room);
        let host = SystemAccountHost;
        let prefix = ["oms".into()];
        let store = super::Store::open(&repo).unwrap();
        if case != "unfinished" {
            let lock = store
                .transaction("panel-main-start", std::time::Duration::from_secs(1))
                .unwrap();
            super::mark_main_account_ready(&repo, "%1", &lock, &svc, &stop).unwrap();
        }

        if case.ends_with("reuse") {
            let start_main = |participant: Option<&str>| {
                let mut args = crate::commands::strings(&[
                    "start",
                    "--provider",
                    provider,
                    "--tool",
                    "panel-main",
                    "--ref",
                    "panel_role=main",
                    "--ref",
                    &format!("panel_owner={provider}"),
                    "--ref",
                    &format!("panel_harness={provider}"),
                    "--ref",
                    &format!("panel_room_id={room}"),
                    "--ref",
                    "panel_model=fixture-model",
                    "--then",
                    "starting",
                    "--then",
                    "working",
                ])
                .into_iter()
                .map(|arg| arg.into_string().unwrap())
                .collect::<Vec<_>>();
                if let Some(member) = participant {
                    args.extend(["--ref".into(), format!("panel_room_participant={member}")]);
                }
                event_id(&repo, &args, &svc, &stop).unwrap()
            };
            let member = start_main(None);
            svc.command(
                NativeCommand::Room,
                &repo,
                &crate::commands::strings(&[
                    "join",
                    "--id",
                    &room,
                    "--participant",
                    &member,
                    "--provider",
                    provider,
                    "--role",
                    "main",
                    "--json",
                ]),
                &stop,
            )
            .unwrap();
            let attempt = if case == "reuse" {
                member.clone()
            } else {
                super::record_main_refs(
                    &repo,
                    &member,
                    &[
                        "panel_native_exit=confirmed".into(),
                        "panel_native_exit_code=0".into(),
                    ],
                    &svc,
                    &stop,
                )
                .unwrap();
                transition_attempt(
                    &repo,
                    &member,
                    &["blocked", "abandoned"],
                    Some("main_swapped"),
                    &svc,
                    &stop,
                )
                .unwrap();
                if case == "swapped-reuse" {
                    let writer = oms_command_room::Room::open(
                        &repo,
                        oms_command_room::Session::default(),
                        &RealCliParsers,
                        &stop,
                        false,
                        false,
                    )
                    .unwrap();
                    writer
                        .swap_main(&room, &member, provider, "fixture-model", &member, provider)
                        .unwrap();
                }
                start_main(Some(&member))
            };
            svc.tags.borrow_mut().extend([
                ("@oms_panel_main_attempt".into(), attempt.clone()),
                ("@oms_panel_room_participant".into(), member.clone()),
                ("@oms_panel_native_pane".into(), "%1".into()),
            ]);
            let pane =
                super::main_account_pane(&repo, "oms-fixture", "%1", provider, &host, &svc, &stop)
                    .unwrap();
            let receipt =
                account_binding::resolve_main_selection(&host, provider, None, "first", "")
                    .unwrap();
            let other =
                account_binding::resolve_main_selection(&host, provider, None, "second", "")
                    .unwrap();
            let lock = store
                .transaction("panel-main-start", std::time::Duration::from_secs(1))
                .unwrap();
            let reuse = |receipt: &super::AccountSelectionReceipt,
                         options: &crate::cli::Options| {
                super::try_reuse_native_main_account(
                    &repo,
                    &prefix,
                    "oms-fixture",
                    provider,
                    receipt,
                    options,
                    &lock,
                    &host,
                    &svc,
                    &stop,
                )
            };
            assert!(reuse(&receipt, &options).is_err());
            account_binding::save_binding(&host, "main", &pane.anchor(&repo, provider), &receipt)
                .unwrap();
            if case == "unproved-reuse" {
                let error = reuse(&receipt, &options).unwrap_err();
                assert!(error.message.contains("Room lineage"));
                assert!(svc.mutation.borrow().is_empty());
                return;
            }
            assert_eq!(pane.attempt, attempt);
            assert_eq!(pane.member, member);
            if case == "swapped-reuse" {
                svc.tags
                    .borrow_mut()
                    .insert("@oms_panel_room_participant".into(), attempt.clone());
                assert!(reuse(&receipt, &options).is_err());
                svc.tags
                    .borrow_mut()
                    .insert("@oms_panel_room_participant".into(), member);
            }
            assert!(reuse(&other, &parse("second", &room)).is_err());
            let mut remapped = receipt.clone();
            remapped.home_digest = other.home_digest;
            assert!(reuse(&remapped, &options).is_err());
            assert!(reuse(&receipt, &parse("first", "foreign-room")).is_err());
            let task = crate::cli::Options::parse(&crate::commands::strings(&[
                "--launch",
                provider,
                "--account",
                "first",
                "--room",
                &room,
                "--task",
                "new work",
            ]))
            .unwrap();
            assert!(reuse(&receipt, &task).is_err());
            let mut replacement = FakeHost::new(host.state_home().unwrap());
            let mut same_pid = host.process_owner(pane.pid).unwrap();
            same_pid.birth.push('x');
            replacement.owners.insert(pane.pid, same_pid);
            assert!(
                super::try_reuse_native_main_account(
                    &repo,
                    &prefix,
                    "oms-fixture",
                    provider,
                    &receipt,
                    &options,
                    &lock,
                    &replacement,
                    &svc,
                    &stop,
                )
                .is_err()
            );
            assert!(svc.mutation.borrow().is_empty());
            assert!(reuse(&receipt, &options).unwrap());
            assert_eq!(*svc.mutation.borrow(), ["select-pane", "select-window"]);
            return;
        }

        let (code, dry, _) = super::dry_run_plan(&repo, source, &options, &svc, &stop).unwrap();
        let plan: Value = serde_json::from_slice(&dry).unwrap();
        assert_eq!(code, 0);
        assert_eq!(plan["account_request"]["id"], alias);
        assert_eq!(plan["executes"], false);
        assert!(!plan.to_string().contains("private-"));
        let capability = super::required_main_capability(provider).unwrap();
        let admitted = super::MainAdmission::admit(capability, &policy, &options).unwrap();
        let shell = super::MainRelay::admit(&repo, &prefix, &admitted)
            .unwrap()
            .command("oms-fixture", Some(&room));
        let words = super::split_shell_words(&shell).unwrap();
        let panel = words.iter().position(|word| word == "panel").unwrap();
        let relayed = crate::cli::Options::parse(
            &words[panel + 1..]
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(relayed.get("account"), Some(alias.as_str()));
        assert_eq!(super::match_panel_command(&shell, &repo), Some(false));
        let watch = super::watch_shell(&repo, &prefix, Some(provider), None, Some(&room), true);
        for command in [&shell, &watch] {
            assert!(!command.contains("ambient-codex"));
            assert!(!command.contains("ambient-claude"));
            assert!(!command.contains("private-"));
        }
        let result = run_fixture_main(&repo, &policy, provider, &options, &svc, &stop, &[]);
        if ["missing", "wrong-room", "unfinished", "lost-pane"].contains(&case.as_str()) {
            let error = result.unwrap_err().message;
            assert!(!error.contains("private-"));
            if case == "lost-pane" {
                let row = last_main(&repo, &RealCliParsers);
                assert_eq!(row["terminal"], true);
                assert_eq!(row["state"], "failed");
                let child: u32 = fs::read_to_string(repo.join(".oms/main-account-child"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                assert!(super::terminals::process(child).is_err());
            } else {
                assert!(!svc.tags.borrow().contains_key("@oms_panel_main_attempt"));
                assert!(usage_rows(&repo).is_empty());
            }
            return;
        }
        assert_eq!(result.unwrap(), 0);
        let bytes = fs::read(repo.join(".oms/main-account-argv")).unwrap();
        let argv: Vec<_> = std::str::from_utf8(&bytes)
            .unwrap()
            .split_terminator('\0')
            .collect();
        let main = last_main(&repo, &RealCliParsers);
        let native = super::native_command(
            &admitted,
            &repo,
            Some(&room),
            main["attempt_id"].as_str(),
            main["refs"]["native_session"].as_str(),
        );
        assert_eq!(argv, native[1..]);
        assert!(
            !argv
                .iter()
                .any(|arg| *arg == "--account" || arg.contains("private-"))
        );
        let pane =
            super::main_account_pane(&repo, "oms-fixture", "%1", provider, &host, &svc, &stop)
                .unwrap();
        let record =
            account_binding::load_verified_binding(&host, "main", &pane.anchor(&repo, provider))
                .unwrap();
        assert_eq!(record.account_id.as_deref(), Some(alias.as_str()));
        assert!(record.account_pinned);
        let persisted = serde_json::to_value(&record).unwrap();
        assert!(!persisted.to_string().contains("private-"));
        if provider == "claude" {
            let pid: u32 = fs::read_to_string(repo.join(".oms/main-account-child"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_ne!(pid, pane.pid);
            assert_eq!(persisted["claude_main_child"]["pid"], pid);
            assert!(
                persisted["claude_main_child"]["birth"]
                    .as_str()
                    .is_some_and(|birth| !birth.is_empty())
            );
            assert!(record.claude_main_child_owner(&host).is_none());
            assert!(!main.to_string().contains("claude_main_child"));
        } else {
            assert!(persisted.get("claude_main_child").is_none());
        }
        assert_eq!(main["terminal"], true);
    }

    #[derive(Default)]
    struct PickerServices(Option<Value>);

    impl PeerExecutor for PickerServices {
        fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            assert_eq!(request.argv.first().unwrap(), "tmux");
            let output = std::process::Command::new("tmux")
                .args(&request.argv[1..])
                .output()
                .unwrap();
            Ok(CommandOutput {
                code: output.status.code().unwrap_or(1),
                stdout: output.stdout,
                stderr: output.stderr,
                cleanup_complete: true,
                output_complete: true,
                limited: false,
            })
        }
    }

    impl Services for PickerServices {
        fn command(
            &self,
            command: NativeCommand,
            repo: &Path,
            args: &[OsString],
            stop: &AtomicBool,
        ) -> Result<CommandOutput> {
            if let Some(view) = &self.0 {
                assert_eq!(command, NativeCommand::Room);
                assert_eq!(args, &["show", "--id", "room-auto", "--exact", "--json"]);
                return Ok(CommandOutput {
                    stdout: serde_json::to_vec(view).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                });
            }
            if command == NativeCommand::ProviderCatalog {
                assert_eq!(args, &["--providers", "gemini", "--json"]);
                return Ok(CommandOutput {
                    stdout: br#"{"providers":[{"provider":"gemini"}]}"#.to_vec(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                });
            }
            let output = RealCliParsers.command(command, repo, args, stop)?;
            if command == NativeCommand::Room
                && matches!(args, [verb, id, _, exact, json]
                    if verb == "show" && id == "--id" && exact == "--exact" && json == "--json")
                && std::env::var("OMS_PI_FIXTURE_STAGE").as_deref() == Ok("main")
            {
                fs::write(repo.join("auto-room-proof"), &output.stdout).unwrap();
            }
            Ok(output)
        }
    }

    struct PickerSocket<'a> {
        binary: &'a Path,
        name: String,
    }

    impl PickerSocket<'_> {
        fn command(&self) -> std::process::Command {
            let mut command = std::process::Command::new(self.binary);
            command
                .env_clear()
                .env("TMUX_TMPDIR", "/tmp")
                .args(["-u", "-L"])
                .arg(&self.name);
            command
        }
    }

    impl Drop for PickerSocket<'_> {
        fn drop(&mut self) {
            let _ = self.command().arg("kill-server").output();
        }
    }

    fn fixture_argv(path: &Path) -> Vec<String> {
        fs::read(path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| std::str::from_utf8(word).unwrap().into())
            .collect()
    }

    #[test]
    fn automatic_orchestrators_require_an_exact_unique_joined_main() {
        let joined = serde_json::json!({
            "participant": "attempt-auto", "provider": "codex", "role": "main", "joined": true
        });
        let open = serde_json::json!({
            "id": "room-auto", "closed": false, "participants": [joined]
        });
        let check = |view| {
            super::current_auto_orchestrator_room(
                Path::new("."),
                "room-auto",
                "attempt-auto",
                "codex",
                &PickerServices(Some(view)),
                &AtomicBool::new(false),
            )
        };
        assert!(check(open.clone()));
        for (field, value) in [
            ("closed", serde_json::json!(true)),
            ("successor", serde_json::json!("room-successor")),
            ("id", serde_json::json!("room-foreign")),
            ("participants", serde_json::json!([joined, joined])),
        ] {
            let mut invalid = open.clone();
            invalid[field] = value;
            assert!(!check(invalid), "accepted room with mismatched {field}");
        }
        for (field, value) in [
            ("participant", serde_json::json!("attempt-foreign")),
            ("provider", serde_json::json!("claude")),
            ("role", serde_json::json!("worker")),
            ("joined", serde_json::json!(false)),
            ("parent", serde_json::json!("foreign-parent")),
        ] {
            let mut invalid = open.clone();
            invalid["participants"][0][field] = value;
            assert!(!check(invalid), "accepted Main with mismatched {field}");
        }
    }

    #[test]
    fn main_reentry_recognizes_only_one_supported_main() {
        let repo = TempRepo::new();
        let entry = format!("bash /old/oms panel --repo '{}'", repo.0.display());
        let recognized = |args: &str| super::match_panel_entry(&format!("{entry} {args}"), &repo.0);
        for harness in ["codex", "claude", "pi", "agy", "antigravity"] {
            assert_eq!(
                recognized(&format!("--launch {harness}")),
                Some(super::PaneMode::Native)
            );
        }
        for args in [
            "--launch gemini",
            "--launch codex,claude",
            "--launch all",
            "--launch ''",
            "--launch gemini --launch=codex",
            "--launch pi --resume fixture-session",
            "--watch --launch claude",
        ] {
            assert!(recognized(args).is_none(), "recognized {args}");
        }
        assert_eq!(recognized("--watch"), Some(super::PaneMode::Watch));
    }

    #[test]
    fn installed_pi_picker_runs_the_gateway_and_opens_the_owned_codex_lead() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const NAME: &str = "native_panel::native_lifecycle_contract::installed_pi_picker_runs_the_gateway_and_opens_the_owned_codex_lead";
        const STAGE: &str = "OMS_PI_FIXTURE_STAGE";
        const TASK: &str = "--model \"$(printf stays-literal)\"; 'quoted'\nnext";
        if let Ok(stage) = std::env::var(STAGE) {
            let repo = PathBuf::from(std::env::var_os("OMS_PI_FIXTURE_REPO").unwrap());
            let source = PathBuf::from(std::env::var_os("HOME").unwrap());
            let policy = oms_panel_core::routing::Policy::bundled().unwrap();
            let stop = AtomicBool::new(false);
            let service = PickerServices::default();
            let prefix = vec!["oms".into()];
            match stage.as_str() {
                "picker" => {
                    let options = crate::cli::Options::parse(&[]).unwrap();
                    let bin = PathBuf::from(std::env::var_os("PATH").unwrap());
                    let codex = bin.join("codex");
                    let unavailable = bin.join("codex-off");
                    fs::rename(&codex, &unavailable).unwrap();
                    assert_eq!(
                        super::pick_main(&stop).unwrap_err().message,
                        "no installed Main provider is available"
                    );
                    assert!(
                        super::main_capability("pi")
                            .unwrap()
                            .require_installed()
                            .unwrap_err()
                            .message
                            .contains("Pi CLI and Codex CLI")
                    );
                    fs::rename(unavailable, codex).unwrap();
                    let oms = bin.join("oms");
                    let unavailable = bin.join("oms-off");
                    fs::rename(&oms, &unavailable).unwrap();
                    assert!(!super::main_capability("pi").unwrap().installed());
                    fs::rename(unavailable, oms).unwrap();

                    let pi = super::main_capability("pi").unwrap();
                    let resume = crate::cli::Options::parse(&crate::commands::strings(&[
                        "--launch",
                        "pi",
                        "--resume",
                        "fixture-resume",
                    ]))
                    .unwrap();
                    let err = super::MainAdmission::plan(pi, &policy, &resume)
                        .err()
                        .unwrap();
                    assert!(err.message.contains("verified Pi session binding"));
                    let opts = crate::cli::Options::parse(&crate::commands::strings(&[
                        "--launch",
                        "pi",
                        &format!("--task={TASK}"),
                    ]))
                    .unwrap();
                    let main = super::MainAdmission::plan(pi, &policy, &opts).unwrap();
                    let explicit = super::native_command(&main, &repo, None, None, None);
                    assert_eq!(
                        &explicit[explicit.len() - 2..],
                        &[
                            "--".to_owned(),
                            format!(
                                "User task (data): {}",
                                serde_json::Value::String(TASK.into())
                            ),
                        ]
                    );
                    super::split_panel(
                        &repo,
                        &prefix,
                        &policy,
                        super::SplitRequest::ReuseOrPick,
                        &options,
                        &service,
                        &stop,
                    )
                    .unwrap();
                }
                "existing-picker" | "existing-cancel" => {
                    let options = crate::cli::Options::parse(&[]).unwrap();
                    let result = super::split_panel(
                        &repo,
                        &prefix,
                        &policy,
                        super::SplitRequest::ReuseOrPick,
                        &options,
                        &service,
                        &stop,
                    );
                    if stage == "existing-cancel" {
                        let error = result.unwrap_err();
                        assert_eq!(error.code, 130, "{}", error.message);
                    } else {
                        assert!(result.unwrap());
                    }
                }
                "main" => {
                    let argv = fixture_argv(&repo.join("relay-argv"));
                    let args: Vec<_> = argv[1..].iter().map(OsString::from).collect();
                    let options = crate::cli::Options::parse(&args).unwrap();
                    if options.get("task").is_some() {
                        assert_eq!(options.get("launch"), Some("agy"));
                        assert_eq!(options.get("task"), Some(TASK));
                    } else {
                        assert_eq!(options.get("launch"), Some("pi"));
                    }
                    assert_eq!(
                        super::run(&repo, &source, &options, &service, &stop)
                            .unwrap()
                            .0,
                        0
                    );
                }
                "alias" => {
                    let options = crate::cli::Options::parse(&crate::commands::strings(&[
                        "--host",
                        "tmux",
                        "--launch",
                        "antigravity",
                        &format!("--task={TASK}"),
                        "--model",
                        "fixture-model",
                    ]))
                    .unwrap();
                    let plan =
                        super::dry_run_plan(&repo, &source, &options, &service, &stop).unwrap();
                    let plan: Value = serde_json::from_slice(&plan.1).unwrap();
                    assert_eq!(plan["harness"], "agy");
                    assert_eq!(plan["provider"], "antigravity");
                    let done = super::run(&repo, &source, &options, &service, &stop);
                    let diagnostic = done.as_ref().err().map_or("ok", |err| err.message.as_str());
                    fs::write(repo.join("alias-entry-status"), diagnostic).unwrap();
                    assert_eq!(done.unwrap().0, 0);
                }
                "refuse" => {
                    assert!(super::managed_session());
                    for (action, value) in [
                        ("--launch", "gemini"),
                        ("--spawn-main", "gemini"),
                        ("--launch", "codex,claude"),
                        ("--spawn-main", "all"),
                    ] {
                        let options = crate::cli::Options::parse(&crate::commands::strings(&[
                            "--host", "tmux", action, value,
                        ]))
                        .unwrap();
                        let error =
                            super::run(&repo, &source, &options, &service, &stop).unwrap_err();
                        assert!(error.message.contains("unsupported launch provider"));
                        if action == "--launch" {
                            assert!(
                                super::dry_run_plan(&repo, &source, &options, &service, &stop)
                                    .is_err()
                            );
                        }
                    }
                    assert!(!repo.join(".oms").exists());
                    assert!(!repo.join("gemini-invoked").exists());
                    for action in ["--launch", "--spawn-main"] {
                        let options = crate::cli::Options::parse(&crate::commands::strings(&[
                            "--host", "inline", action, "claude",
                        ]))
                        .unwrap();
                        let error =
                            super::run(&repo, &source, &options, &service, &stop).unwrap_err();
                        assert!(
                            error
                                .message
                                .contains("Claude main needs the claude CLI on PATH")
                        );
                    }
                    assert!(!repo.join(".oms").exists());
                    fs::write(repo.join("refused"), "ok").unwrap();
                }
                "bad-permissions" => {
                    assert!(super::managed_session());
                    for (host, action) in [
                        ("tmux", "--launch"),
                        ("inline", "--launch"),
                        ("tmux", "--spawn-main"),
                    ] {
                        let options = crate::cli::Options::parse(&crate::commands::strings(&[
                            "--host", host, action, "codex",
                        ]))
                        .unwrap();
                        let error =
                            super::run(&repo, &source, &options, &service, &stop).unwrap_err();
                        assert_eq!(
                            error.message,
                            "OMS_PANEL_MAIN_PERMISSIONS must be full or native"
                        );
                    }
                    assert!(!repo.join(".oms").exists());
                    fs::write(repo.join("bad-permissions"), "ok").unwrap();
                }
                "orchestrator" => {
                    let room = std::env::var("OMS_PI_FIXTURE_ROOM").unwrap();
                    let provider = std::env::var("OMS_PI_FIXTURE_PROVIDER").unwrap();
                    let args =
                        crate::commands::strings(&["--room", &room, "--orchestrator", &provider]);
                    let options = crate::cli::Options::parse(&args).unwrap();
                    crate::orchestrator::run(&repo, &provider, &options, &service, &stop).unwrap();
                }
                _ => panic!("unexpected fixture stage"),
            }
            return;
        }

        let locate = |tool| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|dir| dir.join(tool))
                .find(|path| path.is_file())
        };
        let Some(tmux) = locate("tmux") else {
            eprintln!("skipped isolated tmux fixture (tmux unavailable)");
            return;
        };
        let home = TempRepo::new();
        let repo = TempRepo::new();
        let socket = PickerSocket {
            binary: &tmux,
            name: format!(
                "oms-pi-picker-{}",
                oms_command_plan::storage::random_hex(8).unwrap()
            ),
        };
        let bin = home.0.join("bin");
        fs::create_dir(&bin).unwrap();
        for tool in ["git", "env"] {
            std::os::unix::fs::symlink(locate(tool).unwrap(), bin.join(tool)).unwrap();
        }
        for (name, body) in [
            (
                "tmux",
                // With a cleared locale, tmux rewrites format tabs as underscores.
                r#"case "$1" in attach-session|switch-client) exit 0;; esac
exec "$OMS_PI_FIXTURE_TMUX" -u -L "$OMS_PI_FIXTURE_SOCKET" -f /dev/null "$@""#,
            ),
            (
                "oms",
                r#"case "$1" in
  panel)
    case "$4" in
      --host) export OMS_PI_FIXTURE_REPO="$3"
              printf '%s\000' "$@" > "$OMS_PI_FIXTURE_REPO/relay-argv"
              OMS_PI_FIXTURE_STAGE=main OMS_PANEL_MAIN=unsupported-fixture exec "$OMS_PI_FIXTURE_EXE" --exact "$OMS_PI_FIXTURE_NAME" --nocapture;;
      --room) printf '%s\n' "$OMS_PANEL_MAIN_PERMISSIONS" > "$OMS_PI_FIXTURE_REPO/auto-permissions"
              OMS_PANEL_MAIN_PERMISSIONS=full OMS_PI_FIXTURE_STAGE=orchestrator OMS_PI_FIXTURE_ROOM="$5" OMS_PI_FIXTURE_PROVIDER="$7" exec "$OMS_PI_FIXTURE_EXE" --exact "$OMS_PI_FIXTURE_NAME" --nocapture;;
      --watch) exec /bin/sleep 30;;
    esac;;
  harness)
    printf '%s\000' "$@" > "$OMS_PI_FIXTURE_REPO/bridge-argv"
    while [ "$#" -gt 0 ] && [ "$1" != -- ]; do shift; done
    [ "$#" -gt 0 ] || exit 2
    shift
    exec pi "$@";;
esac
exit 2"#,
            ),
            (
                "pi",
                r#"printf '%s\000' "$@" > "$OMS_PI_FIXTURE_REPO/pi-argv"
printf '%s\n' "$OMS_PANEL_OWNER" "$OMS_PANEL_HARNESS" "$OMS_ROOM_ID" "$OMS_PANEL_MAIN_ATTEMPT" "$OMS_PANEL_MAIN_PERMISSIONS" > "$OMS_PI_FIXTURE_REPO/pi-custody"
exec /bin/sleep 30"#,
            ),
            (
                "codex",
                r#"printf '%s\000' "$@" > "$PWD/codex-argv"
exec /bin/sleep 30"#,
            ),
            ("gemini", r#": > "$PWD/gemini-invoked"; exit 98"#),
        ] {
            let path = bin.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let fixture_exe = std::env::current_exe().unwrap();
        let invoke_picker = |stage: &str, selection: &[u8]| {
            let mut child = Command::new(&fixture_exe)
                .env_clear()
                .env("PATH", &bin)
                .env("HOME", &home.0)
                .env("XDG_CACHE_HOME", home.0.join(".cache"))
                .env("SHELL", "/bin/sh")
                .env("TERM", "xterm")
                .env("OMS_PANEL_MAIN_PERMISSIONS", "native")
                .env("TMUX_TMPDIR", "/tmp")
                .env("OMS_PI_FIXTURE_TMUX", fs::canonicalize(&tmux).unwrap())
                .env("OMS_PI_FIXTURE_SOCKET", &socket.name)
                .env("OMS_PI_FIXTURE_REPO", &repo.0)
                .env("OMS_PI_FIXTURE_EXE", &fixture_exe)
                .env("OMS_PI_FIXTURE_NAME", NAME)
                .env(STAGE, stage)
                .args(["--exact", NAME, "--nocapture"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(selection).unwrap();
            child.wait_with_output().unwrap()
        };
        let result = invoke_picker("picker", b"2\n");
        let log = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "{log}");
        let (_, menu) = log
            .split_once("Choose a Main provider:\n")
            .expect("Main chooser heading");
        let (choices, _) = menu
            .split_once("Selection [1]: ")
            .expect("Main chooser prompt");
        assert_eq!(
            choices.lines().collect::<Vec<_>>(),
            ["  1. codex", "  2. pi"],
            "{log}"
        );

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let panes = socket.command().args([
                "list-panes", "-a", "-F",
                "#{@oms_panel_harness}|#{@oms_panel_owner}|#{@oms_panel_room}|#{@oms_panel_main_attempt}|#{@oms_provider_session}|#{@oms_provider_parent}|#{pane_dead}|#{pane_id}|#{@oms_panel_native_pane}",
            ]).output().unwrap();
            let listing = String::from_utf8_lossy(&panes.stdout);
            if let Ok(custody) = fs::read_to_string(repo.0.join("pi-custody"))
                && let ["codex", "pi", room, main, "native"] =
                    custody.lines().collect::<Vec<_>>().as_slice()
                && listing.lines().any(|line| {
                    line.strip_prefix(&format!("pi|codex|{room}|{main}|||0|"))
                        .and_then(|tail| tail.split_once('|'))
                        .is_some_and(|(pane, native)| pane.starts_with('%') && pane == native)
                })
                && listing
                    .lines()
                    .any(|line| line.starts_with(&format!("||{room}||codex|{main}|0|%")))
                && repo.0.join("codex-argv").is_file()
            {
                let attempt = last_main(&repo.0, &RealCliParsers);
                assert_eq!(attempt["attempt_id"], *main);
                assert_eq!(attempt["provider"], "codex");
                assert_eq!(attempt["refs"]["panel_harness"], "pi");
                assert_eq!(attempt["refs"]["panel_room_id"], *room);
                assert_eq!(attempt["refs"][super::MAIN_PERMISSIONS_REF], "native");
                assert_eq!(attempt["refs"]["panel_native_scheduling"], "unmanaged");
                assert_eq!(attempt["refs"]["panel_host"], "tmux");
                assert_eq!(attempt["refs"]["panel_heartbeat_interval"], "60");
                assert_eq!(attempt["refs"]["panel_heartbeat_ttl"], "300");
                assert!(
                    attempt["refs"]["panel_pane_id"]
                        .as_str()
                        .is_some_and(|v| v.starts_with('%'))
                );
                assert!(
                    attempt["refs"]["panel_tmux_server_pid"]
                        .as_str()
                        .is_some_and(|v| v.parse::<u32>().is_ok_and(|pid| pid > 0))
                );
                assert!(
                    attempt["refs"]["panel_tmux_server_identity"]
                        .as_str()
                        .is_some_and(|v| !v.is_empty())
                );
                assert!(
                    attempt["refs"]["panel_socket_digest"]
                        .as_str()
                        .is_some_and(|v| v.len() == 64)
                );
                assert_eq!(attempt["terminal"], false);
                let room_state = crate::commands::query(
                    &RealCliParsers,
                    NativeCommand::Room,
                    &repo.0,
                    &[
                        "show",
                        "--repo",
                        repo.0.to_str().unwrap(),
                        "--id",
                        room,
                        "--exact",
                        "--json",
                    ],
                    &AtomicBool::new(false),
                )
                .unwrap();
                assert!(crate::rows(&room_state["participants"]).iter().any(|row| {
                    row["participant"] == *main
                        && row["provider"] == "codex"
                        && row["role"] == "main"
                        && row["joined"] == true
                }));
                let auto_proof: Value = serde_json::from_slice(
                    &fs::read(repo.0.join("auto-room-proof")).expect("exact auto-start Room query"),
                )
                .unwrap();
                assert_eq!(auto_proof["id"], *room);
                assert_eq!(auto_proof["closed"], false);
                assert!(auto_proof["successor"].is_null());
                assert!(crate::rows(&auto_proof["participants"]).iter().any(|row| {
                    row["participant"] == *main
                        && row["provider"] == "codex"
                        && row["joined"] == true
                }));
                let bridge = fixture_argv(&repo.0.join("bridge-argv"));
                assert_eq!(&bridge[..3], &["harness", "pi", "--model"]);
                assert!(bridge[3].starts_with("gpt-"));
                assert_eq!(bridge[4], "--effort");
                let pi = fixture_argv(&repo.0.join("pi-argv"));
                assert_eq!(pi.len(), 2);
                assert_eq!(pi[0], "--append-system-prompt");
                assert!(pi[1].contains("Use --owner codex"));
                assert!(pi[1].contains(super::MAIN_STARTUP_PROMPT.trim()));
                assert!(pi[1].contains(&format!("room {room} as {main}")));
                let codex = fixture_argv(&repo.0.join("codex-argv"));
                assert_eq!(
                    fs::read_to_string(repo.0.join("auto-permissions"))
                        .unwrap()
                        .trim(),
                    "native"
                );
                assert_eq!(codex[0], "--model");
                assert!(
                    !codex
                        .iter()
                        .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned Main and Codex lead not ready: {listing}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        let session = super::session_candidates(&repo.0)[0].clone();
        let snapshot = || {
            let output = socket.command().args(["list-panes", "-s", "-t", &format!("={session}"), "-F",
                "#{pane_id}|#{pane_pid}|#{@oms_panel_harness}|#{@oms_panel_main_watch}|#{pane_active}"]).output().unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let before = snapshot();
        let census = socket.command().args(["list-panes", "-a", "-F", "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_dead}\t#{@oms_panel_reader}\t#{pane_start_command}"]).output().unwrap();
        let census = String::from_utf8(census.stdout).unwrap();
        assert!(
            census.lines().any(|line| line
                .splitn(6, '\t')
                .nth(5)
                .is_some_and(|command| super::match_panel_entry(command, &repo.0)
                    == Some(super::PaneMode::Native))),
            "Native pane not discoverable: {census}"
        );
        let rejected = invoke_picker("existing-cancel", b"q\n");
        assert!(rejected.status.success(), "{}; start commands: {}", String::from_utf8_lossy(&rejected.stderr), String::from_utf8_lossy(&socket.command().args(["list-panes", "-a", "-F", "#{session_name}|#{pane_id}|#{pane_dead}|#{@oms_panel_reader}|#{pane_start_command}"]).output().unwrap().stdout));
        assert_eq!(
            snapshot(),
            before,
            "cancelled picker changed live pane selection"
        );
        let attached = invoke_picker("existing-picker", b"2\n");
        let menu = String::from_utf8_lossy(&attached.stderr);
        assert!(attached.status.success(), "{menu}");
        assert!(menu.contains("1. Continue current Main (pi)"), "{menu}");
        assert!(
            menu.contains("2. Open the Board to switch Main (codex)"),
            "{menu}"
        );
        let board_focused = snapshot();
        assert!(
            board_focused
                .lines()
                .any(|line| line.ends_with("||1|1") || line.ends_with("|pi|1|1")),
            "{board_focused}"
        );
        let continued = invoke_picker("existing-picker", b"\n");
        assert!(
            continued.status.success(),
            "{}",
            String::from_utf8_lossy(&continued.stderr)
        );
        assert_eq!(
            snapshot(),
            before,
            "continue did not preserve panes and restore Main focus"
        );
        let trigger = |stage: &str, repo: &Path, session: &str| {
            let argv = [
                fixture_exe.to_str().unwrap(),
                "--exact",
                NAME,
                "--nocapture",
            ];
            let clean_permissions = if stage == "bad-permissions" {
                ""
            } else {
                " -u OMS_PANEL_MAIN_PERMISSIONS"
            };
            let command = format!(
                "exec env -u OMS_ROOM_ID -u OMS_ROOM_REPO -u OMS_STATE_REPO{clean_permissions} {}",
                argv.map(crate::commands::shell_quote).join(" ")
            );
            let mut run = socket.command();
            run.args([
                "new-window",
                "-d",
                "-t",
                &format!("={session}:"),
                "-c",
                repo.to_str().unwrap(),
                "-e",
                &format!("{STAGE}={stage}"),
                "-e",
                &format!("OMS_PI_FIXTURE_REPO={}", repo.display()),
            ]);
            if stage == "bad-permissions" {
                run.args(["-e", "OMS_PANEL_MAIN_PERMISSIONS=invalid"]);
            }
            let output = run.arg(command).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let wait_for = |condition: &mut dyn FnMut() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !condition() {
                assert!(
                    Instant::now() < deadline,
                    "private tmux fixture did not reach expected state"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        };
        let rejected = TempRepo::new();
        let managed = super::session_candidates(&rejected.0)[1].clone();
        assert_ne!(managed, session);
        let control = socket
            .command()
            .args([
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-s",
                &managed,
                "-n",
                "control",
                "exec /bin/sleep 30",
            ])
            .output()
            .unwrap();
        assert!(
            control.status.success(),
            "{}",
            String::from_utf8_lossy(&control.stderr)
        );
        for argv in [
            [
                "set-option",
                "-t",
                &format!("={managed}:"),
                "@oms_panel_repo",
                rejected.0.to_str().unwrap(),
            ],
            [
                "set-environment",
                "-t",
                &format!("={managed}"),
                "OMS_PANEL_SESSION",
                &managed,
            ],
        ] {
            assert!(
                socket
                    .command()
                    .args(argv)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        trigger("refuse", &rejected.0, &managed);
        wait_for(&mut || rejected.0.join("refused").is_file());
        trigger("bad-permissions", &rejected.0, &managed);
        wait_for(&mut || rejected.0.join("bad-permissions").is_file());
        wait_for(&mut || {
            let windows = socket
                .command()
                .args([
                    "list-windows",
                    "-t",
                    &format!("={managed}:"),
                    "-F",
                    "#{window_id}",
                ])
                .output()
                .unwrap();
            windows.status.success() && windows.stdout == control.stdout
        });
        assert!(!rejected.0.join(".oms").exists() && !rejected.0.join("gemini-invoked").exists());

        let agy = bin.join("agy");
        fs::write(&agy, r#"#!/bin/sh
printf '%s\000' "$@" > "$PWD/agy-argv"
printf '%s\n' "$OMS_PANEL_OWNER" "$OMS_PANEL_HARNESS" "$OMS_ROOM_ID" "$OMS_PANEL_MAIN_ATTEMPT" > "$PWD/agy-custody"
exec /bin/sleep 30
"#).unwrap();
        fs::set_permissions(agy, fs::Permissions::from_mode(0o755)).unwrap();
        let alias = TempRepo::new();
        trigger("alias", &alias.0, &session);
        wait_for(&mut || {
            if let Ok(status) = fs::read_to_string(alias.0.join("alias-entry-status"))
                && !status.is_empty()
            {
                assert_eq!(status, "ok");
                return fs::read_to_string(alias.0.join("agy-custody"))
                    .is_ok_and(|text| text.lines().count() == 4);
            }
            false
        });
        let agy = fixture_argv(&alias.0.join("agy-argv"));
        let custody = fs::read_to_string(alias.0.join("agy-custody")).unwrap();
        let custody_lines = custody.lines().collect::<Vec<_>>();
        let ["antigravity", "agy", room, attempt] = custody_lines.as_slice() else {
            panic!("unexpected alias Main custody: {custody}");
        };
        assert_eq!(agy[0], "--prompt-interactive");
        let task = format!(
            "User task (data): {}",
            serde_json::Value::String(TASK.into())
        );
        assert_eq!(agy[1].matches(&task).count(), 1);
        assert!(agy[1].contains(&format!("room {room} as {attempt}")));
        assert_eq!(
            agy[2..],
            ["--dangerously-skip-permissions", "--model", "fixture-model"]
        );
        let main = last_main(&alias.0, &RealCliParsers);
        assert_eq!(main["attempt_id"], *attempt);
        assert_eq!(main["refs"]["panel_harness"], "agy");
        assert_eq!(main["refs"][super::MAIN_PERMISSIONS_REF], "full");
        assert_eq!(main["provider"], "antigravity");
    }

    fn usage_rows(repo: &Path) -> Vec<serde_json::Value> {
        fs::read(repo.join(".oms/usage/receipts.jsonl"))
            .unwrap_or_default()
            .split(|byte| *byte == b'\n')
            .filter(|row| !row.is_empty())
            .map(|row| serde_json::from_slice(row).unwrap())
            .collect()
    }
    fn last_main(repo: &Path, svc: &RealCliParsers) -> Value {
        let raw = fs::read(repo.join(".oms/lifecycle/events.jsonl")).unwrap();
        let created = raw
            .split(|b| *b == b'\n')
            .filter(|row| !row.is_empty())
            .map(|row| serde_json::from_slice::<serde_json::Value>(row).unwrap())
            .rfind(|row| row["event_type"] == "attempt.created")
            .unwrap();
        crate::commands::query(
            svc,
            NativeCommand::Events,
            repo,
            &[
                "show",
                "--attempt",
                created["attempt_id"].as_str().unwrap(),
                "--json",
            ],
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn run_fixture_main(
        repo: &Path,
        policy: &oms_panel_core::routing::Policy,
        harness: &str,
        options: &crate::cli::Options,
        services: &dyn Services,
        stop: &AtomicBool,
        prefix: &[String],
    ) -> Result<i32> {
        let main = super::MainAdmission::admit(
            super::required_main_capability(harness)?,
            policy,
            options,
        )?;
        super::run_native(repo, &main, services, stop, prefix)
    }

    #[test]
    fn native_main_launch_preserves_physical_custody_and_unknown_evidence() {
        const NAME: &str = "native_panel::native_lifecycle_contract::native_main_launch_preserves_physical_custody_and_unknown_evidence";
        const CASE: &str = "OMS_NATIVE_MAIN_TEST_CASE";
        if std::env::var_os("OMS_NATIVE_MAIN_TEST_ENTRY").is_some() {
            let repo = PathBuf::from(std::env::var_os("OMS_STATE_REPO").unwrap());
            let call_id = std::env::var("OMS_PROVIDER_CALL_ID").unwrap();
            let rows = usage_rows(&repo);
            let pending = rows.last().unwrap();
            assert_eq!(pending["call_id"], call_id);
            assert_eq!(
                pending["operation_id"],
                std::env::var("OMS_OPERATION_ID").unwrap()
            );
            assert_eq!(pending["phase"], "pending");
            assert_eq!(
                pending["provider"],
                std::env::var("OMS_PANEL_OWNER").unwrap()
            );
            assert_eq!(pending["selected_model"], "fixture-model");
            assert_eq!(
                pending["attempt_id"],
                std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap()
            );
            assert!(std::env::var_os("OMS_ATTEMPT_ID").is_none());
            assert!(std::env::var_os("OMS_RESUME_SESSION").is_none());
            if std::env::var(CASE).is_ok_and(|case| case.starts_with("resume-")) {
                assert_eq!(
                    pending["cumulative"]["session_id"],
                    "11111111-1111-1111-1111-111111111111"
                );
                assert_eq!(pending["cumulative"]["baseline_unknown"], true);
            } else {
                assert!(
                    pending["cumulative"].is_null(),
                    "a fresh launch ignores stale resume context"
                );
            }
            let argv = fs::read_to_string(repo.join(".oms/native-argv")).unwrap();
            assert!(argv.lines().any(|arg| arg == "fixture-model"));
            if std::env::var(CASE).is_ok_and(|case| case.starts_with("resume-")) {
                assert!(
                    argv.lines()
                        .any(|arg| arg == "11111111-1111-1111-1111-111111111111")
                );
            }
            let registry: serde_json::Value = serde_json::from_slice(
                &fs::read(repo.join(".oms/artifacts/panel-dispatch/reservations.json")).unwrap(),
            )
            .unwrap();
            let intent = &registry["physical"]["calls"][&call_id];
            assert_eq!(intent["state"], "ready");
            assert_eq!(intent["usage_ordinal"], pending["ordinal"]);
            assert_eq!(intent["request"]["identity"]["call_id"], call_id);
            let mut settings = oms_lock::Settings::from_env().unwrap();
            settings.mode = oms_lock::Mode::Flock;
            let _dispatch = oms_lock::Lock::in_root(
                repo.clone(),
                Path::new(".oms/artifacts/panel-dispatch.lock"),
                settings.clone(),
            )
            .unwrap()
            .try_acquire()
            .unwrap();
            assert!(matches!(
                oms_lock::Lock::in_root(
                    repo.clone(),
                    Path::new(".oms/artifacts/panel-main-session.lock"),
                    settings
                )
                .unwrap()
                .try_acquire(),
                Err(oms_lock::Error::Busy)
            ));
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(repo.join(".oms/native-entry-count"))
                .and_then(|mut file| std::io::Write::write_all(&mut file, b"entry\n"))
                .unwrap();
            if fs::read_to_string(repo.join(".oms/native-main-wait")).is_ok() {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            if std::env::var(CASE).as_deref() == Ok("exit125") {
                std::process::exit(125);
            }
            return;
        }
        let Ok(case) = std::env::var(CASE) else {
            for case in [
                "plain",
                "alias",
                "wrong-state",
                "invalid-binding",
                "fresh-codex",
                "resume-codex",
                "resume-bound",
                "resume-unverified",
                "resume-live",
                "resume-foreign",
                "resume-descendants",
                "resume-ambiguous",
                "bounded",
                "exit125",
                "spawn-error",
            ] {
                let home = TempRepo::new();
                let repo = TempRepo::new();
                let bin = home.0.join("bin");
                fs::create_dir(&bin).unwrap();
                let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                    .map(|dir| dir.join("git"))
                    .find(|path| path.is_file())
                    .unwrap();
                std::os::unix::fs::symlink(git.canonicalize().unwrap(), bin.join("git")).unwrap();
                let script = format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OMS_STATE_REPO/.oms/native-argv\"\nOMS_NATIVE_MAIN_TEST_ENTRY=1 exec \"$OMS_NATIVE_MAIN_TEST_EXE\" --exact {NAME} --nocapture\n"
                );
                for provider in ["claude", "codex"] {
                    fs::write(
                        bin.join(provider),
                        if case == "spawn-error" {
                            format!("#!{}/missing-interpreter\n", bin.display())
                        } else {
                            script.clone()
                        },
                    )
                    .unwrap();
                    fs::set_permissions(bin.join(provider), fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                let mut cmd = std::process::Command::new("/bin/bash");
                cmd.env_clear();
                for key in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
                    if let Some(value) = std::env::var_os(key) {
                        cmd.env(key, value);
                    }
                }
                cmd.args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "umask 022; exec \"$@\"",
                    "native-main-fixture",
                ])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("HOME", &home.0)
                .env("XDG_CACHE_HOME", home.0.join(".cache"))
                .env("PATH", &bin)
                .env(CASE, case)
                .env("OMS_NATIVE_MAIN_TEST_ROOT", &repo.0)
                .env("OMS_NATIVE_MAIN_TEST_EXE", std::env::current_exe().unwrap());
                match case {
                    "alias" => {
                        let alias = home.0.join("state-alias");
                        std::os::unix::fs::symlink(&repo.0, &alias).unwrap();
                        cmd.env("OMS_STATE_REPO", alias);
                    }
                    "wrong-state" => {
                        cmd.env("OMS_STATE_REPO", &home.0);
                    }
                    "invalid-binding" => {
                        cmd.env("OMS_CAMPAIGN_MANIFEST_SHA256", "");
                    }
                    "fresh-codex" | "resume-codex" | "resume-bound" | "resume-unverified"
                    | "resume-ambiguous" => {
                        cmd.env("OMS_RESUME_SESSION", "stale-session");
                    }
                    "bounded" => {
                        let manifest = serde_json::json!({"problem":{"file":"problem.txt","sha256":"a".repeat(64)},"base":"b".repeat(40),
                            "verify":{"command":"true","timeout_s":30,"inputs":[]},"candidate_formats":["text"],
                            "groups":[{"id":"group","formulation":"fixture","k":1,"routes":[{"provider":"claude","model":"fixture-model"}]}],
                            "budget":{"max_concurrent":4,"total_calls":10,"deadline_s":60,"max_waves":2,"stall_waves":2,
                            "verifier":{"concurrency":1,"timeout_s":30,"max_artifact_bytes":1024},"usage":{"max_tokens":60,"per_call":{"tokens":60}}}});
                        let raw = oms_command_plan::json::canonical(
                            &oms_command_plan::json::parse(&serde_json::to_vec(&manifest).unwrap())
                                .unwrap(),
                        )
                        .unwrap();
                        let digest = oms_panel_core::digest(&raw);
                        let directory = format!(".oms/plan/campaigns/{}", &digest[..16]);
                        let operation =
                            format!("res_{}", oms_command_plan::storage::random_hex(16).unwrap());
                        let store = oms_command_plan::storage::Store::open(&repo.0).unwrap();
                        store
                            .publish_once(format!("{directory}/manifest.json"), &raw, 1024 * 1024)
                            .unwrap();
                        let now = chrono::DateTime::from_timestamp(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs() as i64,
                            0,
                        )
                        .unwrap()
                        .to_rfc3339();
                        for row in [
                            serde_json::json!({"type":"campaign_started","budget":manifest["budget"],"started_at":now}),
                            serde_json::json!({"type":"reservation","id":operation,"provider":"claude","kind":"seat","reserved_usage":{"tokens":60,"cost_microusd":null}}),
                        ] {
                            store
                                .append(
                                    format!("{directory}/events.jsonl"),
                                    &oms_command_plan::json::parse(
                                        &serde_json::to_vec(&row).unwrap(),
                                    )
                                    .unwrap(),
                                )
                                .unwrap();
                        }
                        cmd.env("OMS_CAMPAIGN_MANIFEST_SHA256", &digest)
                            .env("OMS_OPERATION_ID", operation)
                            .env("OMS_GOAL_ID", &digest[..16]);
                    }
                    _ => {}
                }
                let output = cmd.output().unwrap();
                let body = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.status.success(), "{case}: {body}");
                assert!(body.contains("1 passed;"), "{case}: {body}");
            }
            return;
        };
        let repo = PathBuf::from(std::env::var_os("OMS_NATIVE_MAIN_TEST_ROOT").unwrap());
        let source = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let policy =
            oms_panel_core::routing::Policy::load(&source.join("config/panel-routing.json"))
                .unwrap();
        let harness = if case == "fresh-codex" || case.starts_with("resume-") {
            "codex"
        } else {
            "claude"
        };
        let mut args = crate::commands::strings(&["--launch", harness, "--model", "fixture-model"]);
        if case.starts_with("resume-") {
            args.extend(crate::commands::strings(&[
                "--resume",
                "11111111-1111-1111-1111-111111111111",
            ]));
        }
        let options = crate::cli::Options::parse(&args).unwrap();
        let stop = AtomicBool::new(false);
        let svc = RealCliParsers;
        let mut expected_room = None;
        if ["resume-bound", "resume-descendants"].contains(&case.as_str()) {
            assert_eq!(
                run_fixture_main(&repo, &policy, harness, &options, &svc, &stop, &[]).unwrap(),
                0
            );
            expected_room = last_main(&repo, &svc)["refs"]["panel_room_id"]
                .as_str()
                .map(str::to_owned);
            if case == "resume-descendants" {
                let owner = last_main(&repo, &svc)["attempt_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                svc.command(
                    NativeCommand::Room,
                    &repo,
                    &crate::commands::strings(&[
                        "join",
                        "--id",
                        expected_room.as_deref().unwrap(),
                        "--participant",
                        "existing-worker",
                        "--provider",
                        "claude",
                        "--role",
                        "worker",
                        "--parent",
                        &owner,
                        "--owns",
                        "src",
                        "--json",
                    ]),
                    &stop,
                )
                .unwrap()
                .success()
                .unwrap();
            }
        }
        if ["resume-live", "resume-foreign"].contains(&case.as_str()) {
            let room = super::ensure_room(&repo, None, &svc, &stop).unwrap();
            let digest = oms_panel_core::digest(b"11111111-1111-1111-1111-111111111111");
            let proof = if case == "resume-foreign" {
                "f".repeat(32)
            } else {
                digest[..32].to_owned()
            };
            let old = event_id(
                &repo,
                &[
                    "start",
                    "--provider",
                    "codex",
                    "--tool",
                    "panel-main",
                    "--ref",
                    "panel_role=main",
                    "--ref",
                    "panel_owner=codex",
                    "--ref",
                    "panel_harness=codex",
                    "--ref",
                    &format!("panel_room_id={room}"),
                    "--ref",
                    &format!("panel_session_digest={proof}"),
                    "--then",
                    "starting",
                    "--then",
                    "working",
                ]
                .map(str::to_owned),
                &svc,
                &stop,
            )
            .unwrap();
            svc.command(
                NativeCommand::Room,
                &repo,
                &crate::commands::strings(&[
                    "join",
                    "--id",
                    &room,
                    "--participant",
                    &old,
                    "--provider",
                    "codex",
                    "--role",
                    "main",
                    "--native-session",
                    "11111111-1111-1111-1111-111111111111",
                    "--json",
                ]),
                &stop,
            )
            .unwrap()
            .success()
            .unwrap();
            if case == "resume-foreign" {
                transition_attempt(
                    &repo,
                    &old,
                    &["verifying", "review", "done"],
                    None,
                    &svc,
                    &stop,
                )
                .unwrap();
            }
            expected_room = Some(room);
        }
        if ["resume-unverified", "resume-ambiguous"].contains(&case.as_str()) {
            for n in 0..if case == "resume-ambiguous" { 2 } else { 1 } {
                let room = super::ensure_room(&repo, None, &svc, &stop).unwrap();
                svc.command(
                    NativeCommand::Room,
                    &repo,
                    &crate::commands::strings(&[
                        "join",
                        "--id",
                        &room,
                        "--participant",
                        &format!("old-main-{n}"),
                        "--provider",
                        "codex",
                        "--role",
                        "main",
                        "--native-session",
                        "11111111-1111-1111-1111-111111111111",
                        "--json",
                    ]),
                    &stop,
                )
                .unwrap()
                .success()
                .unwrap();
                expected_room = Some(room);
            }
        }
        let before_room = expected_room
            .as_ref()
            .map(|id| fs::read(repo.join(format!(".oms/threads/{id}.jsonl"))).unwrap());
        let result = run_fixture_main(&repo, &policy, harness, &options, &svc, &stop, &[]);
        if case == "resume-ambiguous" {
            assert!(result.unwrap_err().message.contains("multiple rooms"));
            assert!(usage_rows(&repo).is_empty());
            assert!(!repo.join(".oms/native-entry-count").exists());
            return;
        }
        if [
            "resume-unverified",
            "resume-live",
            "resume-foreign",
            "resume-descendants",
        ]
        .contains(&case.as_str())
        {
            assert!(
                result
                    .unwrap_err()
                    .message
                    .contains(if case == "resume-descendants" {
                        "joined descendants"
                    } else {
                        "unverified"
                    })
            );
            assert_eq!(
                usage_rows(&repo).len(),
                if case == "resume-descendants" { 2 } else { 0 }
            );
            assert_eq!(
                repo.join(".oms/native-entry-count").exists(),
                case == "resume-descendants"
            );
            assert_eq!(
                fs::read(repo.join(format!(
                    ".oms/threads/{}.jsonl",
                    expected_room.as_deref().unwrap()
                )))
                .unwrap(),
                before_room.unwrap()
            );
            let room = crate::commands::query(
                &svc,
                NativeCommand::Room,
                &repo,
                &["show", "--id", expected_room.as_deref().unwrap(), "--json"],
                &stop,
            )
            .unwrap();
            assert_eq!(
                room["participants"].as_array().unwrap().len(),
                if case == "resume-descendants" { 2 } else { 1 }
            );
            assert_eq!(room["participants"][0]["joined"], Value::from(true));
            if case == "resume-descendants" {
                let core = oms_command_room::Room::open(
                    &repo,
                    oms_command_room::Session::default(),
                    &svc,
                    &stop,
                    false,
                    false,
                )
                .unwrap();
                let id = expected_room.as_deref().unwrap();
                let state = core.status(id, false).unwrap();
                let overlaps = core
                    .scope_overlaps(id, &state, "next-main", &["src".into()])
                    .unwrap();
                assert!(
                    overlaps
                        .iter()
                        .any(|p| p.text("participant") == "existing-worker")
                );
                let next = core.rollover(id, None).unwrap();
                let carried = core.status(&next, false).unwrap();
                assert!(
                    carried
                        .list("participants")
                        .iter()
                        .any(|p| p.text("participant") == "existing-worker" && p.flag("joined"))
                );
            }
            return;
        }
        if case == "spawn-error" {
            assert!(result.unwrap_err().message.contains("native launch failed"));
            assert!(!repo.join(".oms/native-entry-count").exists());
            let rows = usage_rows(&repo);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["phase"], "pending");
            assert_eq!(rows[1]["phase"], "unknown");
            assert!(rows[1]["cleanup_complete"].is_null());
            assert!(rows[1]["source_sha256"].is_null());
            assert_eq!(last_main(&repo, &svc)["state"], Value::from("failed"));
            return;
        }
        if ["wrong-state", "invalid-binding"].contains(&case.as_str()) {
            assert_eq!(result.unwrap_err().code, 2);
            assert!(!repo.join(".oms/native-entry-count").exists());
            assert!(usage_rows(&repo).is_empty());
            if case == "invalid-binding" {
                assert_eq!(last_main(&repo, &svc)["state"], Value::from("failed"));
            }
            return;
        }
        assert_eq!(result.unwrap(), if case == "exit125" { 125 } else { 0 });
        if let Some(room) = expected_room {
            assert_eq!(
                last_main(&repo, &svc)["refs"]["panel_room_id"],
                Value::from(room)
            );
        }
        if case == "resume-bound" {
            let room_id = last_main(&repo, &svc)["refs"]["panel_room_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let core = oms_command_room::Room::open(
                &repo,
                oms_command_room::Session::default(),
                &svc,
                &stop,
                false,
                false,
            )
            .unwrap();
            let rows = core.records(&room_id).unwrap();
            let state = oms_command_room::project(&rows).unwrap();
            let members = state.list("participants");
            assert_eq!(members.len(), 2);
            assert!(!members[0].flag("joined"));
            assert!(members[1].flag("joined"));
            assert_eq!(state["owner"], members[1]["participant"]);
            core.updates(&room_id, members[1].text("participant"), "", 65536, 200)
                .unwrap();
            let resumed = rows
                .iter()
                .rev()
                .find(|r| r["room_event"].text("kind") == "main-resume")
                .unwrap();
            let before = fs::read(repo.join(format!(".oms/threads/{room_id}.jsonl"))).unwrap();
            assert!(
                !core
                    .append(&room_id, &resumed["room_event"], resumed.text("text"))
                    .unwrap()
                    .0
            );
            assert_eq!(
                fs::read(repo.join(format!(".oms/threads/{room_id}.jsonl"))).unwrap(),
                before
            );
            let mut changed = resumed["room_event"].clone();
            changed.set("kind", "join");
            assert!(oms_command_room::event_shape(&changed).is_err());
            changed.set("kind", "main-resume");
            changed.set("label", "changed resume payload");
            assert!(core.append(&room_id, &changed, "changed resume").is_err());
            assert_eq!(
                fs::read(repo.join(format!(".oms/threads/{room_id}.jsonl"))).unwrap(),
                before
            );
            let successor = core.rollover(&room_id, None).unwrap();
            let carried = core.status(&successor, false).unwrap();
            assert!(
                carried
                    .list("participants")
                    .iter()
                    .any(|p| p["participant"] == members[1]["participant"] && p.flag("joined"))
            );
        }
        let rows = usage_rows(&repo);
        let finished = rows.last().unwrap();
        assert_eq!(finished["phase"], "unknown");
        assert!(finished["source_sha256"].is_null());
        assert!(finished["cleanup_complete"].is_null());
        assert!(finished["provider_usage"]["total_tokens"].is_null());
        assert_eq!(finished["cost"]["status"], "unknown");
        assert_eq!(
            fs::read_to_string(repo.join(".oms/native-entry-count")).unwrap(),
            if case == "resume-bound" {
                "entry\nentry\n"
            } else {
                "entry\n"
            }
        );
        if case == "bounded" {
            let before =
                fs::read(repo.join(".oms/artifacts/panel-dispatch/reservations.json")).unwrap();
            assert_eq!(
                run_fixture_main(&repo, &policy, harness, &options, &svc, &stop, &[])
                    .unwrap_err()
                    .code,
                3
            );
            assert_eq!(
                fs::read(repo.join(".oms/artifacts/panel-dispatch/reservations.json")).unwrap(),
                before
            );
            assert_eq!(
                fs::read_to_string(repo.join(".oms/native-entry-count")).unwrap(),
                "entry\n"
            );
        }
        if case != "plain" {
            return;
        }

        let registry = repo.join(".oms/artifacts/panel-dispatch/reservations.json");
        let before = fs::read(&registry).unwrap();
        let receipts = fs::read(repo.join(".oms/usage/receipts.jsonl")).unwrap();
        fs::write(&registry, b"{corrupt").unwrap();
        assert!(run_fixture_main(&repo, &policy, harness, &options, &svc, &stop, &[]).is_err());
        assert_eq!(fs::read(&registry).unwrap(), b"{corrupt");
        assert_eq!(
            fs::read(repo.join(".oms/usage/receipts.jsonl")).unwrap(),
            receipts
        );
        assert_eq!(
            fs::read_to_string(repo.join(".oms/native-entry-count")).unwrap(),
            "entry\n"
        );
        assert_eq!(last_main(&repo, &svc)["state"], Value::from("failed"));
        fs::write(registry, before).unwrap();
        fs::write(repo.join(".oms/native-main-wait"), b"wait").unwrap();
        std::thread::scope(|scope| {
            let running = scope
                .spawn(|| run_fixture_main(&repo, &policy, harness, &options, &svc, &stop, &[]));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                if fs::read_to_string(repo.join(".oms/native-entry-count"))
                    .is_ok_and(|value| value.lines().count() == 2)
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stop.store(true, Ordering::Release);
            assert_eq!(running.join().unwrap().unwrap(), 130);
        });
        assert_eq!(last_main(&repo, &svc)["state"], Value::from("cancelled"));
        let rows = usage_rows(&repo);
        assert_eq!(rows.last().unwrap()["phase"], "unknown");
        assert!(rows.last().unwrap()["cleanup_complete"].is_null());
    }

    #[test]
    fn native_main_event_and_room_calls_match_real_cli_parsers() {
        const CHILD: &str = "OMS_NATIVE_LIFECYCLE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = TempRepo::new();
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.env_clear();
            for key in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
                if let Some(val) = std::env::var_os(key) {
                    cmd.env(key, val);
                }
            }
            let output = cmd
                .args([
                    "--exact",
                    "native_panel::native_lifecycle_contract::native_main_event_and_room_calls_match_real_cli_parsers",
                    "--nocapture",
                ])
                .env("HOME", &home.0)
                .env("XDG_CACHE_HOME", home.0.join(".cache"))
                .env(CHILD, "1")
                .output()
                .unwrap();
            let body = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{body}");
            assert!(body.contains("1 passed;"), "{body}");
            return;
        }
        let repo = TempRepo::new();
        let stop = AtomicBool::new(false);
        let svc = RealCliParsers;
        let room_id = super::ensure_room(&repo.0, None, &svc, &stop).unwrap();
        assert!(room_id.starts_with("room-"), "{room_id}");

        let start_args = vec![
            "start".into(),
            "--provider".into(),
            "claude".into(),
            "--tool".into(),
            "panel-main".into(),
            "--ref".into(),
            "panel_role=main".into(),
            "--ref".into(),
            format!("panel_room_id={room_id}"),
            "--then".into(),
            "starting".into(),
            "--then".into(),
            "working".into(),
        ];
        let attempt = event_id(&repo.0, &start_args, &svc, &stop).unwrap();
        assert!(attempt.starts_with("att_"), "{attempt}");

        svc.command(
            NativeCommand::Room,
            &repo.0,
            &crate::commands::strings(&[
                "join",
                "--id",
                &room_id,
                "--participant",
                &attempt,
                "--provider",
                "claude",
                "--role",
                "main",
                "--label=-dash-safe",
                "--native-session",
                "11111111-1111-1111-1111-111111111111",
                "--json",
            ]),
            &stop,
        )
        .unwrap()
        .success()
        .unwrap();

        heartbeat_attempt(&repo.0, &attempt, &svc, &stop).unwrap();
        transition_attempt(
            &repo.0,
            &attempt,
            &["verifying", "review", "done"],
            Some("native_exit"),
            &svc,
            &stop,
        )
        .unwrap();

        let shown = crate::commands::query(
            &svc,
            NativeCommand::Events,
            &repo.0,
            &["show", "--attempt", &attempt, "--json"],
            &stop,
        )
        .unwrap();
        assert_eq!(shown["state"], Value::from("done"));
        assert_eq!(shown["terminal"], Value::from(true));
    }

    #[test]
    fn staged_entry_prefix_carries_oms_dispatch_and_matches_panes() {
        const NAME: &str = "native_panel::native_lifecycle_contract::staged_entry_prefix_carries_oms_dispatch_and_matches_panes";
        const CASE: &str = "OMS_NATIVE_RELAY_CASE";
        if std::env::var_os("OMS_NATIVE_RELAY_ENTRY").is_some() {
            let names = [
                "OMS_STATE_REPO",
                "OMS_ROOM_REPO",
                "OMS_ROOM_ID",
                "OMS_PANEL_AUTO_ORCHESTRATORS",
            ]
            .iter()
            .chain(super::NATIVE_CONTEXT_ENV);
            let snapshot = names
                .map(|name| ((*name).to_owned(), std::env::var(name).ok()))
                .collect::<std::collections::BTreeMap<_, _>>();
            fs::write(
                std::env::var_os("OMS_NATIVE_RELAY_OUT").unwrap(),
                serde_json::to_vec(&snapshot).unwrap(),
            )
            .unwrap();
            return;
        }
        let Ok(case) = std::env::var(CASE) else {
            for case in ["absent", "present", "empty", "non-utf8"] {
                let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
                cmd.env_clear()
                    .args(["--exact", NAME, "--nocapture"])
                    .env(CASE, case);
                for key in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
                    if let Some(value) = std::env::var_os(key) {
                        cmd.env(key, value);
                    }
                }
                if case == "present" {
                    for name in super::NATIVE_CONTEXT_ENV {
                        cmd.env(name, format!("caller {name} 'quoted'\nnext"));
                    }
                } else if case == "empty" {
                    cmd.env("OMS_CAMPAIGN_MANIFEST_SHA256", "");
                } else if case == "non-utf8" {
                    use std::os::unix::ffi::OsStringExt;
                    cmd.env("CODEX_HOME", OsString::from_vec(vec![0xff]));
                }
                let output = cmd.output().unwrap();
                assert!(
                    output.status.success(),
                    "{case}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
            }
            return;
        };
        let dir = TempRepo::new();
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let staged_dir = dir.0.join("target/oms-native").join(rev);
        fs::create_dir_all(&staged_dir).unwrap();
        let exe = staged_dir.join("oms");
        fs::write(&exe, format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$OMS_NATIVE_RELAY_ARGS\"\nOMS_NATIVE_RELAY_ENTRY=1 exec \"$OMS_NATIVE_RELAY_EXE\" --exact {NAME} --nocapture\n")).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(staged_dir.join("manifest.json"), "{}").unwrap();

        let prefix = super::entry_prefix_for(
            &dir.0,
            &crate::executable::Entry {
                path: exe.clone(),
                script: false,
            },
        );
        assert_eq!(
            prefix,
            vec![
                exe.to_string_lossy().into_owned(),
                "--oms-dispatch".to_owned(),
                dir.0.to_string_lossy().into_owned(),
                rev.to_owned(),
                "--".to_owned(),
            ]
        );
        assert!(super::test_entry_matches(&dir.0, &prefix));

        let launch = |task: Option<&str>| -> Result<String> {
            let mut args = crate::commands::strings(&["--launch", "codex"]);
            if let Some(task) = task {
                args.push(format!("--task={task}").into());
            }
            let options = crate::cli::Options::parse(&args)?;
            let policy = oms_panel_core::routing::Policy::bundled().unwrap();
            let admitted = super::MainAdmission::plan(
                super::main_capability("codex").unwrap(),
                &policy,
                &options,
            )?;
            let (environment, argv) = super::MainRelay::parts(&dir.0, &prefix, &admitted)?;
            Ok(super::MainRelay {
                execution: dir.0.clone(),
                environment,
                argv,
            }
            .command("oms-test", Some("room-1234")))
        };
        let launched = launch(None);
        if case == "non-utf8" {
            assert!(
                launched
                    .unwrap_err()
                    .message
                    .contains("CODEX_HOME is not UTF-8")
            );
            return;
        }
        let launch_cmd = launched.unwrap();
        let snapshot = dir.0.join("relay.json");
        let recorded_args = dir.0.join("relay-args");
        let mut shell = std::process::Command::new("sh");
        shell
            .env_clear()
            .args(["-c", &launch_cmd])
            .env("OMS_PANEL_AUTO_ORCHESTRATORS", "stale")
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("OMS_NATIVE_RELAY_EXE", std::env::current_exe().unwrap())
            .env("OMS_NATIVE_RELAY_OUT", &snapshot)
            .env("OMS_NATIVE_RELAY_ARGS", &recorded_args);
        for name in ["OMS_STATE_REPO", "OMS_ROOM_REPO", "OMS_ROOM_ID"]
            .iter()
            .chain(super::NATIVE_CONTEXT_ENV)
        {
            shell.env(name, "stale-server-value");
        }
        let output = shell.output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let seen: serde_json::Value =
            serde_json::from_slice(&fs::read(&snapshot).unwrap()).unwrap();
        assert_eq!(seen["OMS_STATE_REPO"], dir.0.to_str().unwrap());
        assert_eq!(seen["OMS_ROOM_REPO"], dir.0.to_str().unwrap());
        assert_eq!(seen["OMS_ROOM_ID"], "room-1234");
        assert!(seen["OMS_PANEL_AUTO_ORCHESTRATORS"].is_null());
        let auto_cmd = super::enable_auto_providers(&launch_cmd);
        assert_eq!(super::match_panel_command(&auto_cmd, &dir.0), Some(false));
        let output = std::process::Command::new("sh")
            .env_clear()
            .args(["-c", &auto_cmd])
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("OMS_NATIVE_RELAY_EXE", std::env::current_exe().unwrap())
            .env("OMS_NATIVE_RELAY_OUT", &snapshot)
            .env("OMS_NATIVE_RELAY_ARGS", &recorded_args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let auto_seen: serde_json::Value =
            serde_json::from_slice(&fs::read(&snapshot).unwrap()).unwrap();
        assert_eq!(auto_seen["OMS_PANEL_AUTO_ORCHESTRATORS"], "1");
        for name in super::NATIVE_CONTEXT_ENV {
            assert_eq!(
                seen[name],
                serde_json::to_value(std::env::var(name).ok()).unwrap(),
                "{name}"
            );
        }
        let captured = fs::read(recorded_args).unwrap();
        let captured: Vec<_> = captured
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| OsString::from(String::from_utf8(arg.to_vec()).unwrap()))
            .collect();
        assert_eq!(
            &captured[..prefix.len() - 1],
            &prefix[1..].iter().map(OsString::from).collect::<Vec<_>>()
        );
        assert_eq!(captured[prefix.len() - 1], "panel");
        assert!(!dir.0.join(".oms/usage/receipts.jsonl").exists());
        assert_eq!(super::match_panel_command(&launch_cmd, &dir.0), Some(false));
        for task in [
            "--foo",
            "--help",
            "--한글 'quoted'\nnext",
            "ordinary task",
            "",
        ] {
            let command = launch(Some(task)).unwrap();
            let words = super::split_shell_words(&command).unwrap();
            let panel = words.iter().position(|word| word == "panel").unwrap();
            let args: Vec<OsString> = words[panel + 1..].iter().map(OsString::from).collect();
            let options = crate::cli::Options::parse(&args).expect("pane reentry options");
            assert_eq!(options.get("task"), Some(task));
            assert!(!options.has("help"));
            assert_eq!(super::match_panel_command(&command, &dir.0), Some(false));
        }
        let watch_cmd = super::watch_shell(
            &dir.0,
            &prefix,
            Some("codex"),
            Some("oms-test"),
            Some("room-fixture"),
            false,
        );
        assert_eq!(super::match_panel_command(&watch_cmd, &dir.0), Some(true));

        let hud = super::hud_status_right(&prefix, &dir.0);
        assert!(hud.contains("--oms-dispatch"));
        assert!(hud.contains("hud --tmux --repo"));

        // Incomplete --oms-dispatch prefix is rejected instead of misreading flags.
        let malformed = format!(
            "'{}' --oms-dispatch panel --repo '{}'",
            exe.display(),
            dir.0.display()
        );
        assert_eq!(super::match_panel_command(&malformed, &dir.0), None);
    }

    #[test]
    fn check_legacy_main_excludes_callers_own_pending_tmux_window() {
        struct PendingWindowMock {
            reference: Option<Value>,
            terminal: bool,
            matrix: bool,
            control_only: bool,
            closed_room: bool,
        }
        impl PeerExecutor for PendingWindowMock {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                let args: Vec<String> = request
                    .argv
                    .iter()
                    .map(|v| v.to_string_lossy().into_owned())
                    .collect();
                let stdout = if args.iter().any(|a| a == "has-session") {
                    Vec::new()
                } else if args.iter().any(|a| a == "show-options") {
                    b"/repo\n".to_vec()
                } else if args.last().is_some_and(|arg| arg == "@oms_panel_room") {
                    b"room-1\n".to_vec()
                } else if args.iter().any(|a| a == "list-windows") {
                    if self.control_only {
                        b"@1\tcodex\troom-1\tunrelated\tcodex\tcodex\t\t.\n".to_vec()
                    } else if self.matrix {
                        b"@1\tcodex\troom-1\tmain-1\tcodex\tcodex\t\t.\n".to_vec()
                    } else {
                        b"@2\t\troom-1\t\tAGY Orchestrator\t\tantigravity\t.\n@1\tcodex\troom-1\t\tcodex\tcodex\t\t.\n".to_vec()
                    }
                } else {
                    return Err(oms_panel_core::Error::new("unexpected tmux command"));
                };
                Ok(CommandOutput {
                    code: 0,
                    stdout,
                    stderr: Vec::new(),
                    cleanup_complete: true,
                    output_complete: true,
                    limited: false,
                })
            }
        }
        impl Services for PendingWindowMock {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                match command {
                    NativeCommand::Events => Ok(CommandOutput {
                        code: 0,
                        stdout: if self.matrix {
                            let mut row = serde_json::json!({"attempt_id":"main-1","tool":"panel-main","terminal":self.terminal,"refs":{"panel_room_id":"room-1"}});
                            if let Some(reference) = &self.reference {
                                row["refs"]["panel_room_participant"] = reference.clone();
                            }
                            serde_json::to_vec(&vec![row]).unwrap()
                        } else {
                            b"[]".to_vec()
                        },
                        stderr: Vec::new(),
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    }),
                    NativeCommand::Room => Ok(CommandOutput {
                        code: 0,
                        stdout: serde_json::to_vec(&serde_json::json!({
                            "id":"room-1",
                            "closed":self.closed_room,
                            "participants": if self.matrix {
                                serde_json::json!([{"participant":"main-1","role":"main","joined":true}])
                            } else {
                                serde_json::json!([])
                            },
                        }))
                        .unwrap(),
                        stderr: Vec::new(),
                        cleanup_complete: true,
                        output_complete: true,
                        limited: false,
                    }),
                    _ => Err(oms_panel_core::Error::new("unexpected native command")),
                }
            }
        }

        let dir = TempRepo::new();
        let stop = AtomicBool::new(false);
        let pending = PendingWindowMock {
            reference: None,
            terminal: false,
            matrix: false,
            control_only: false,
            closed_room: false,
        };
        let pane = super::PanelPane {
            session: "oms-test".into(),
            window: "@1".into(),
            pane: "%1".into(),
            mode: super::PaneMode::Native,
            signature: "fixture".into(),
        };
        super::check_reattach_room(&dir.0, &pane, Some("room-1"), &pending, &stop).unwrap();
        assert!(
            super::check_reattach_room(&dir.0, &pane, Some("room-other"), &pending, &stop)
                .unwrap_err()
                .message
                .contains("different room")
        );
        let closed = PendingWindowMock {
            closed_room: true,
            ..pending
        };
        assert!(
            super::check_reattach_room(&dir.0, &pane, Some("room-1"), &closed, &stop)
                .unwrap_err()
                .message
                .contains("unavailable or closed")
        );
        super::check_reattach_room(&dir.0, &pane, None, &closed, &stop).unwrap();
        let pending = PendingWindowMock {
            closed_room: false,
            ..closed
        };
        assert!(super::check_legacy_main(&dir.0, "oms-test", None, None, &pending, &stop).is_err());
        super::check_legacy_main(&dir.0, "oms-test", Some("@1"), None, &pending, &stop).unwrap();
        let empty = crate::cli::Options::parse(&[]).unwrap();
        for terminal in [false, true] {
            for reference in [None, Some(Value::from("")), Some(Value::from("foreign"))] {
                let svc = PendingWindowMock {
                    reference: reference.clone(),
                    terminal,
                    matrix: true,
                    control_only: false,
                    closed_room: false,
                };
                let expected = usize::from(!terminal || reference == Some(Value::from("foreign")));
                assert_eq!(
                    super::live_mains(&dir.0, "oms-test", Some("room-1"), None, &svc, &stop)
                        .unwrap(),
                    expected,
                    "terminal={terminal} reference={reference:?}"
                );
                assert_eq!(
                    super::ensure_no_active_main(&dir.0, "oms-test", true, &empty, &svc, &stop)
                        .is_err(),
                    expected == 1,
                    "terminal={terminal} reference={reference:?}"
                );
                if !terminal {
                    let svc = PendingWindowMock {
                        reference: reference.clone(),
                        terminal,
                        matrix: true,
                        control_only: true,
                        closed_room: false,
                    };
                    assert_eq!(
                        super::live_mains(&dir.0, "oms-test", Some("room-1"), None, &svc, &stop)
                            .unwrap(),
                        1 + usize::from(reference != Some(Value::from("foreign")))
                    );
                }
            }
        }
    }

    #[test]
    fn check_legacy_main_releases_crashed_main_while_holding_panel_locks() {
        const CHILD: &str = "OMS_RELEASE_LOCKS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = TempRepo::new();
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.env_clear();
            for key in ["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ"] {
                if let Some(val) = std::env::var_os(key) {
                    cmd.env(key, val);
                }
            }
            let output = cmd
                .args([
                    "--exact",
                    "native_panel::native_lifecycle_contract::check_legacy_main_releases_crashed_main_while_holding_panel_locks",
                    "--nocapture",
                ])
                .env("HOME", &home.0)
                .env("XDG_CACHE_HOME", home.0.join(".cache"))
                .env(CHILD, "1")
                .output()
                .unwrap();
            let body = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{body}");
            assert!(body.contains("1 passed;"), "{body}");
            return;
        }
        let dir = TempRepo::new();
        let stop = AtomicBool::new(false);
        let svc = RealCliParsers;
        let mut dead = std::process::Command::new("/bin/true").spawn().unwrap();
        let dead_pid = dead.id();
        let _ = dead.wait();
        let start_args = vec![
            "start".into(),
            "--provider".into(),
            "codex".into(),
            "--tool".into(),
            "panel-main".into(),
            "--ref".into(),
            "panel_role=main".into(),
            "--ref".into(),
            "panel_owner=codex".into(),
            "--ref".into(),
            "panel_harness=codex".into(),
            "--ref".into(),
            "panel_host=tmux".into(),
            "--ref".into(),
            "panel_pane_id=%1".into(),
            "--ref".into(),
            format!("panel_socket_digest={}", "a".repeat(64)),
            "--ref".into(),
            format!("panel_tmux_server_pid={dead_pid}"),
            "--ref".into(),
            "panel_tmux_server_identity=dead:1".into(),
            "--ref".into(),
            format!("panel_wrapper_pid={dead_pid}"),
            "--ref".into(),
            "panel_wrapper_identity=dead:2".into(),
            "--ref".into(),
            format!("panel_native_pid={dead_pid}"),
            "--ref".into(),
            "panel_native_identity=dead:3".into(),
            "--ref".into(),
            "panel_room_id=room-crashed".into(),
            "--then".into(),
            "starting".into(),
            "--then".into(),
            "working".into(),
        ];
        let crashed = event_id(&dir.0, &start_args, &svc, &stop).unwrap();
        let store = super::Store::open(&dir.0).unwrap();
        let start_lock = store
            .transaction("panel-main-start", std::time::Duration::from_secs(1))
            .unwrap();
        let sess_lock = store
            .transaction("panel-main-session", std::time::Duration::from_secs(1))
            .unwrap();
        super::check_legacy_main(
            &dir.0,
            "oms-crashed",
            None,
            Some((&start_lock, &sess_lock)),
            &svc,
            &stop,
        )
        .unwrap();
        let shown = crate::commands::query(
            &svc,
            NativeCommand::Events,
            &dir.0,
            &["show", "--attempt", &crashed, "--json"],
            &stop,
        )
        .unwrap();
        assert_eq!(shown["state"], Value::from("abandoned"));
        assert_eq!(shown["terminal"], Value::from(true));
        assert_eq!(shown["reason_code"], Value::from("panel_main_released"));
    }

    #[test]
    fn unverified_or_unmatched_existing_main_attaches_instead_of_refusing() {
        struct ExistingMainMock {
            repo: PathBuf,
            discoverable: bool,
            selected: std::cell::RefCell<Vec<String>>,
        }
        impl PeerExecutor for ExistingMainMock {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                let args: Vec<_> = request.argv.iter().map(|v| v.to_str().unwrap()).collect();
                let stdout = match args.as_slice() {
                    ["tmux", "list-panes", "-a", "-F", _] => {
                        if self.discoverable {
                            let repo = crate::commands::shell_quote(self.repo.to_str().unwrap());
                            format!(
                                "oms-fixture\t@1\t%1\t0\t\toms panel --repo {repo} --launch codex\noms-fixture\t@1\t%2\t0\t\toms panel --repo {repo} --watch"
                            )
                        } else {
                            String::new()
                        }
                    }
                    ["tmux", "display-message", "-p", "-t", "%1", _] => {
                        // Missing @oms_panel_main_attempt / @oms_panel_native_pane -> unverified
                        "oms-fixture\t$1\t@1\t%1\t1\t0\tcodex\tcodex\troom-1\t\t\t\t.".into()
                    }
                    ["tmux", "list-windows", "-t", "=oms-fixture", "-F", _] => format!(
                        "@1\tcodex\troom-1\tatt-1\tcodex\tcodex\t\t{}\t.",
                        self.repo.display()
                    ),
                    [
                        "tmux",
                        "show-option",
                        "-w",
                        "-v",
                        "-t",
                        "%1",
                        "@oms_panel_room",
                    ] => "room-1".into(),
                    [
                        "tmux",
                        action @ ("select-pane" | "select-window"),
                        "-t",
                        target,
                    ] => {
                        self.selected
                            .borrow_mut()
                            .push(format!("{action}:{target}"));
                        String::new()
                    }
                    _ => {
                        return Err(oms_panel_core::Error::new(format!(
                            "unexpected mock tmux {args:?}"
                        )));
                    }
                };
                Ok(CommandOutput {
                    stdout: stdout.into_bytes(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        impl Services for ExistingMainMock {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Room);
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&serde_json::json!({
                        "id": "room-1",
                        "closed": false,
                        "participants": [],
                    }))
                    .unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }

        let dir = TempRepo::new();
        let stop = AtomicBool::new(false);
        let store = super::Store::open(&dir.0).unwrap();
        let lock = store
            .transaction("panel-main-start", std::time::Duration::from_secs(1))
            .unwrap();
        let prefix = vec!["oms".into()];

        // 1. Discoverable native pane that cannot pass full Main-swap verification
        // returns Unverified so startup attaches via try_reuse_native_main.
        let unverified = ExistingMainMock {
            repo: dir.0.clone(),
            discoverable: true,
            selected: Default::default(),
        };
        assert!(matches!(
            super::verified_startup_main(&dir.0, "oms-fixture", None, &unverified, &stop).unwrap(),
            super::StartupMainProbe::Unverified
        ));
        assert!(
            super::try_reuse_native_main(
                &dir.0,
                &prefix,
                "oms-fixture",
                None,
                &lock,
                &unverified,
                &stop
            )
            .unwrap()
        );
        assert_eq!(
            *unverified.selected.borrow(),
            ["select-pane:%1", "select-window:%1"]
        );

        // 2. Tagged Main window whose start command wasn't recognized still attaches
        // cleanly, while refusing a mismatched --room.
        let tagged_only = ExistingMainMock {
            repo: dir.0.clone(),
            discoverable: false,
            selected: Default::default(),
        };
        assert!(
            super::try_reuse_native_main(
                &dir.0,
                &prefix,
                "oms-fixture",
                Some("room-other"),
                &lock,
                &tagged_only,
                &stop
            )
            .unwrap_err()
            .message
            .contains("different room")
        );
        assert!(
            super::try_reuse_native_main(
                &dir.0,
                &prefix,
                "oms-fixture",
                Some("room-1"),
                &lock,
                &tagged_only,
                &stop
            )
            .unwrap()
        );
        assert_eq!(*tagged_only.selected.borrow(), ["select-window:@1"]);
    }
}

#[cfg(test)]
mod main_selection_tests {
    use super::{MAIN_CAPABILITIES, pick_from};
    use std::io::{PipeReader, PipeWriter, Write};
    use std::os::fd::AsFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn environment_supports_typed_main_selectors_and_board_only() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        for (raw, harness) in [
            ("codex", "codex"),
            (" claude ", "claude"),
            ("agy", "agy"),
            ("antigravity", "agy"),
            ("pi", "pi"),
        ] {
            assert_eq!(
                super::environment_main(OsStr::new(raw))
                    .unwrap()
                    .unwrap()
                    .harness,
                harness
            );
        }
        assert!(
            super::environment_main(OsStr::new(" \t"))
                .unwrap()
                .is_none()
        );
        assert!(super::environment_main(OsStr::new("unsupported")).is_err());
        assert!(super::environment_main(OsStr::from_bytes(&[0xff])).is_err());
    }

    fn input(bytes: &[u8]) -> (PipeReader, PipeWriter) {
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(bytes).unwrap();
        (reader, writer)
    }

    fn pick(bytes: &[u8], stop: &AtomicBool) -> (oms_panel_core::Result<&'static str>, String) {
        let (reader, writer) = input(bytes);
        drop(writer);
        let mut out = Vec::new();
        let choices: Vec<_> = MAIN_CAPABILITIES.iter().collect();
        let picked = pick_from(&choices, reader.as_fd(), stop, &mut out).map(|main| main.harness);
        (picked, String::from_utf8(out).unwrap())
    }

    fn assert_cancelled(picked: oms_panel_core::Result<&str>) {
        let error = picked.unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (130, "Main selection cancelled")
        );
    }

    #[test]
    fn main_selection_reads_lines_and_reprompts() {
        let stop = AtomicBool::new(false);
        for (input_bytes, expected) in [(b"\n".as_slice(), 0), (b"2\n".as_slice(), 1)] {
            let (reader, writer) = input(input_bytes);
            drop(writer);
            let mut out = Vec::new();
            let choices = [
                "Continue current Main (pi)",
                "Open the Board to switch Main (codex)",
            ];
            let index = super::pick_number(&choices, reader.as_fd(), &stop, &mut out).unwrap();
            assert_eq!(index, expected);
            let out = String::from_utf8(out).unwrap();
            assert!(out.contains("1. Continue current Main (pi)"));
            assert!(out.contains("2. Open the Board to switch Main (codex)"));
        }
        assert_eq!(pick(b"2\n", &stop).0.unwrap(), "claude");
        assert_eq!(pick(b"\n", &stop).0.unwrap(), "codex");
        for bytes in [&b"9\nx\n1\n"[..], b"\xff\n0\n1\n"] {
            let (picked, out) = pick(bytes, &stop);
            assert_eq!(picked.unwrap(), "codex");
            assert_eq!(out.matches("Choose one of the listed numbers.").count(), 2);
            assert_eq!(out.matches("Selection [1]: ").count(), 3);
        }
        assert_cancelled(pick(b"", &stop).0);
        assert_cancelled(pick(b"2", &stop).0);
        assert_cancelled(pick(b"q\n", &stop).0);
        let (reader, _writer) = input(b"");
        assert!(super::main_picker_foreground(reader.as_fd()).is_err());
        stop.store(true, Ordering::Release);
        assert_cancelled(pick(b"2\n", &stop).0);
    }

    #[test]
    fn main_selection_cancels_when_stop_is_set_while_waiting() {
        for bytes in [&b""[..], b"2"] {
            let stop = AtomicBool::new(false);
            let (reader, _writer) = input(bytes);
            let started = Instant::now();
            let picked = std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(150));
                    stop.store(true, Ordering::Release);
                });
                let choices: Vec<_> = MAIN_CAPABILITIES.iter().collect();
                pick_from(&choices, reader.as_fd(), &stop, &mut Vec::new()).map(|main| main.harness)
            });
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_cancelled(picked);
        }
    }
}
