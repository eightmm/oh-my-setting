//! Native interactive main panel launch, reopen, split construction and window spawning.
use crate::{
    Error, Output, Result, Services, cli::Options, json, parent_only, require, rows, text,
};
use oms_panel_core::{
    NativeCommand, Store, Value, host, release, routing, services::ExecutionRequest, state,
    terminals,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_LIVE_MAINS: usize = 1;
const LAUNCH_HARNESSES: [&str; 3] = ["claude", "codex", "agy"];
// Historical Pi windows still participate in ownership and sole-Main checks.
const RECORDED_HARNESSES: &[&str] = &["claude", "codex", "pi", "agy"];
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
const MAIN_STARTUP_PROMPT: &str = "As Overall Main, turn the user's goals into bounded \
assignments with completion checks. Choose a role for each assignment. Use researcher for \
read-only investigation, builder for scoped implementation, reviewer for independent review, \
and verifier for read-only checks. In the Main pane of a managed OMS tmux panel, choose a \
provider separately from the role. Open or reuse it with oms panel --orchestrator <provider>, \
then use the same command with --task to send the role, scope, and checks on one line. When \
provider orchestrators are unavailable, including in standalone inline or Herdr sessions, use \
oms panel --dispatch <role> --prompt <assignment> where authorized. Coordinate dependencies \
and avoid overlapping writes. Review the returned evidence, verify the integrated result, and \
report completion and blockers to the user. Keep your own implementation to necessary integration \
and unblocking. Preserve project rules and approval requirements. Wait for a user task before assigning work.";
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
    "OMS_PANEL_MAIN_PERMISSIONS",
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

/// Produce `--dry-run` launch plan JSON without spawning processes or opening panels.
pub fn dry_run_plan(
    repo: &Path,
    source: &Path,
    options: &Options,
    _services: &dyn Services,
    _stop: &AtomicBool,
) -> Result<Output> {
    let harness = match options.get("launch") {
        Some(raw) => Some(normalize_harness(raw)?),
        None => None,
    };
    if harness == Some("agy") && options.has("resume") {
        return Err(Error::new(
            "Agy resume needs a verified native session binding; start a fresh Agy main",
        ));
    }
    let split = wants_split(options);
    let policy = load_policy(source)?;
    let resolution = harness.map(|h| resolve_main(&policy, h, options.get("model")));
    let argv = if let Some(h) = harness {
        Some(native_command(
            &policy,
            h,
            repo,
            options.get("resume"),
            options.get("model"),
            options.get("task"),
            None,
            None,
            None,
        )?)
    } else {
        None
    };
    let host = if split {
        "tmux"
    } else if options.get("host") == Some("herdr") || host::herdr_active() {
        "herdr"
    } else {
        "inline"
    };
    let room = crate::commands::room(options).unwrap_or_else(|| "new on launch".into());
    let plan = json!({
        "schema": 1,
        "kind": "oms-panel-launch",
        "repo": repo,
        "layout": if split { "split" } else { "inline" },
        "host": host,
        "provider": resolution.as_ref().map(|r| r.owner.as_str()),
        "harness": harness,
        "max_live_mains": MAX_LIVE_MAINS,
        "argv": argv,
        "model": resolution.as_ref().and_then(|r| r.model.as_deref()),
        "room": room,
        "environment_keys": ENV_KEYS,
        "executes": false,
    });
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
    if wants_split(options) {
        let (session, owner) = resolve_or_legacy_session(repo, services, stop)?;
        if let Some(owner) = owner {
            require(
                owner == repo.to_string_lossy().as_ref(),
                "existing panel belongs to another checkout; existing sessions preserved",
            )?;
            if let Some(base) = existing_main_pane(repo, &session, services, stop)? {
                let prefix = entry_prefix(source)?;
                let policy = load_policy(source)?;
                split_panel(
                    repo,
                    &prefix,
                    &policy,
                    SplitRequest::ReuseExisting(&base),
                    options,
                    services,
                    stop,
                )?;
                return Ok(Some((0, vec![], vec![])));
            }
            require(
                live_mains(repo, &session, None, None, services, stop)? == 0,
                "another main is live or unverified; existing sessions preserved",
            )?;
        }
    }
    let selected = match std::env::var("OMS_PANEL_MAIN") {
        Ok(raw) => main_selection(Some(&raw))?,
        Err(std::env::VarError::NotPresent) => MainSelection::Prompt,
        Err(_) => return Err(Error::new("OMS_PANEL_MAIN must be UTF-8")),
    };
    let harness = match selected {
        MainSelection::Launch(harness) => Some(harness),
        MainSelection::Board => None,
        MainSelection::Prompt => {
            let Some(mut terminal) = crate::input::TerminalInput::open()? else {
                return Err(Error::new(
                    "Main picker needs a foreground terminal; use --launch codex, claude or agy, or --watch for the board",
                ));
            };
            let installed = LAUNCH_HARNESSES.map(which_exists);
            pick_main(
                &mut |wait| terminal.wait(wait),
                &mut std::io::stdout().lock(),
                &installed,
                stop,
            )?
        }
    };
    match harness {
        Some(harness) => {
            oms_command_peer::admission::interactive_state(repo).map_err(admission_error)?;
            launch(repo, source, harness, options, services, stop).map(Some)
        }
        None => Ok(None),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum MainSelection {
    Prompt,
    Board,
    Launch(&'static str),
}

fn main_selection(raw: Option<&str>) -> Result<MainSelection> {
    match raw.map(str::trim) {
        None => Ok(MainSelection::Prompt),
        Some("") => Ok(MainSelection::Board),
        Some(raw) => normalize_harness(raw).map(MainSelection::Launch),
    }
}

fn pick_main(
    input: &mut dyn FnMut(Duration) -> Result<Vec<crate::input::Key>>,
    output: &mut dyn Write,
    installed: &[bool; 3],
    stop: &AtomicBool,
) -> Result<Option<&'static str>> {
    use crate::input::Key;
    writeln!(output, "Overall Main · choose a native conversation").map_err(Error::new)?;
    for (index, harness) in LAUNCH_HARNESSES.iter().enumerate() {
        let availability = if installed[index] {
            ""
        } else {
            " (CLI not on PATH)"
        };
        writeln!(output, "  {} {}{availability}", index + 1, capitalize(harness))
            .map_err(Error::new)?;
    }
    writeln!(output, "Enter / Esc / Ctrl-D: board only · Ctrl-C: quit")
        .map_err(Error::new)?;
    output.flush().map_err(Error::new)?;
    loop {
        let interrupted = || Error {
            code: 130,
            ..Error::new("Main selection interrupted")
        };
        if stop.load(Ordering::Acquire) {
            return Err(interrupted());
        }
        let keys = input(Duration::from_millis(100))?;
        if stop.load(Ordering::Acquire) {
            return Err(interrupted());
        }
        for key in keys {
            match key {
                Key::Enter | Key::Escape | Key::Eof => return Ok(None),
                Key::Char(key @ '1'..='3') => {
                    let index = usize::from(key as u8 - b'1');
                    if installed[index] {
                        return Ok(Some(LAUNCH_HARNESSES[index]));
                    }
                    writeln!(
                        output,
                        "{} CLI is not on PATH; choose another Main or Enter for the board",
                        capitalize(LAUNCH_HARNESSES[index])
                    )
                    .map_err(Error::new)?;
                    output.flush().map_err(Error::new)?;
                }
                _ => {}
            }
        }
    }
}

fn existing_main_pane(
    repo: &Path,
    session: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<PanelPane>> {
    let mut candidates: Vec<_> = discover_panel_panes(repo, services, stop)?
        .into_iter()
        .filter(|pane| pane.session == session && pane.mode == PaneMode::Native)
        .collect();
    require(
        candidates.len() <= 1,
        "multiple native Main panes found; reopen from the intended OMS window",
    )?;
    Ok(candidates.pop())
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
    reopen_pane(repo, &prefix, candidates[0], services, stop)
}

fn reopen_pane(
    repo: &Path,
    prefix: &[String],
    base: &PanelPane,
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
        let cmd = watch_shell(repo, prefix, None, Some(&base.session), room.as_deref());
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
    let prefix = entry_prefix(source)?;
    let harness = normalize_harness(raw_provider)?;
    let policy = load_policy(source)?;
    let model = options.get("model");
    let resolution = resolve_main(&policy, harness, model);
    ensure_main_installed(harness)?;
    require(
        which_exists("tmux"),
        &format!("starting a main needs tmux and the {harness} CLI on PATH"),
    )?;

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
    release_dead_mains(repo, services, stop)?;
    require(
        live_mains(repo, &session, Some(&ident), None, services, stop)? < MAX_LIVE_MAINS,
        "another main is live or unverified; finish it before starting another",
    )?;
    {
        let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
        check_legacy_main(repo, &session, None, services, stop)?;
        sess_lock.check()?;
    }

    start_lock.check()?;
    let pane = add_main_window(
        repo,
        &prefix,
        &policy,
        &session,
        harness,
        "graph",
        false,
        options.get("resume"),
        model,
        options.get("task"),
        &ident,
        services,
        stop,
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
        "owner": resolution.owner,
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
    let prefix = entry_prefix(source)?;
    let harness = normalize_harness(raw_provider)?;
    if harness == "agy" && options.has("resume") {
        return Err(Error::new(
            "Agy resume needs a verified native session binding; start a fresh Agy main",
        ));
    }
    require(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "interactive panel needs a terminal; use --json or --dry-run",
    )?;
    let policy = load_policy(source)?;
    ensure_main_installed(harness)?;

    if wants_split(options) {
        require(
            which_exists("tmux"),
            "native chat board requires tmux on PATH",
        )?;
        split_panel(
            repo,
            &prefix,
            &policy,
            SplitRequest::StartNew(harness),
            options,
            services,
            stop,
        )?;
        return Ok((0, vec![], vec![]));
    }
    let code = run_native(repo, &policy, harness, options, services, stop)?;
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
    ReuseExisting(&'a PanelPane),
    StartNew(&'a str),
}

fn split_panel(
    repo: &Path,
    prefix: &[String],
    policy: &routing::Policy,
    request: SplitRequest<'_>,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    let store = Store::open(repo)?;
    let start_lock = store.transaction("panel-main-start", Duration::from_secs(3))?;
    let (session, owner) = resolve_or_legacy_session(repo, services, stop)?;
    let repo_str = repo.to_string_lossy().into_owned();
    if let Some(o) = &owner {
        require(
            o == &repo_str,
            &format!(
                "tmux session {session} is not this checkout's OMS panel; close it or use --layout inline"
            ),
        )?;
    }

    let harness = match request {
        SplitRequest::StartNew(harness) => harness,
        SplitRequest::ReuseExisting(expected) => {
            require(
                owner.is_some(),
                "existing Main panel disappeared; retry selection",
            )?;
            let base = existing_main_pane(repo, &session, services, stop)?
                .ok_or_else(|| Error::new("existing Main disappeared; retry selection"))?;
            require(
                base.session == expected.session
                    && base.window == expected.window
                    && base.pane == expected.pane
                    && base.signature == expected.signature,
                "existing Main changed; existing sessions preserved",
            )?;
            start_lock.check()?;
            check_reattach_room(repo, &base, options.get("room"), services, stop)?;
            start_lock.check()?;
            reopen_pane(repo, prefix, &base, services, stop)?;
            start_lock.check()?;
            terminals::tmux(
                services,
                repo,
                &crate::commands::strings(&["select-window", "-t", &base.pane]),
                stop,
            )?;
            drop(start_lock);
            return attach_session(&session);
        }
    };
    let chosen_room = crate::commands::room(options);
    if let Some(resume) = options.get("resume") {
        let bound_rooms = native_resume_binding(
            &resolve_main(policy, harness, options.get("model")).owner,
            repo,
            resume,
            services,
            stop,
        )?;
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
        release_dead_mains(repo, services, stop)?;
        require(
            live_mains(repo, &session, Some(&ident), None, services, stop)? < MAX_LIVE_MAINS,
            "another main is live or unverified; finish it before starting another",
        )?;
        {
            let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
            check_legacy_main(repo, &session, None, services, stop)?;
            sess_lock.check()?;
        }
        start_lock.check()?;
        let pane = add_main_window(
            repo,
            prefix,
            policy,
            &session,
            harness,
            options.get("view").unwrap_or("auto"),
            options.has("attention-only"),
            options.get("resume"),
            options.get("model"),
            options.get("task"),
            &ident,
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
    } else {
        {
            let sess_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
            check_legacy_main(repo, &session, None, services, stop)?;
            sess_lock.check()?;
        }
        let ident = if let Some(resume) = options.get("resume") {
            let bound = native_resume_binding(
                &resolve_main(policy, harness, options.get("model")).owner,
                repo,
                resume,
                services,
                stop,
            )?;
            let candidate = chosen_room.or_else(|| bound.into_keys().next());
            start_lock.check()?;
            ensure_room(repo, candidate.as_deref(), services, stop)?
        } else {
            start_lock.check()?;
            ensure_room(repo, chosen_room.as_deref(), services, stop)?
        };
        let execution = state::native_repository(repo, None)?;
        let cmd = launch_command(
            repo,
            prefix,
            &session,
            harness,
            options.get("resume"),
            options.get("model"),
            options.get("task"),
            Some(&ident),
        )?;
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
        argv.push(execution.into());
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
            policy,
            harness,
            options.get("view").unwrap_or("auto"),
            options.has("attention-only"),
            options.get("model"),
            &ident,
            services,
            repo,
            stop,
        )?;
        let owner = resolve_main(policy, harness, options.get("model")).owner;
        let watch = watch_shell(repo, prefix, Some(&owner), None, Some(&ident));
        board_split(&pane, repo, &watch, true, services, stop)?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-pane", "-t", &pane]),
            stop,
        )?;
    }

    drop(start_lock);
    attach_session(&session)
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

fn run_native(
    repo: &Path,
    policy: &routing::Policy,
    harness: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<i32> {
    let harness = normalize_harness(harness)?;
    oms_command_peer::admission::interactive_state(repo).map_err(admission_error)?;
    let resolution = resolve_main(policy, harness, options.get("model"));
    let store = Store::open(repo)?;
    let session_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
    let session = std::env::var("OMS_PANEL_SESSION").unwrap_or_default();
    if !session.is_empty() {
        let pending = pending_window_id(repo, services, stop);
        check_legacy_main(repo, &session, pending.as_deref(), services, stop)?;
        session_lock.check()?;
    }

    let execution = state::native_repository(repo, None)?;
    let resume = options.get("resume");
    let chosen_room = crate::commands::room(options);
    let (room_id, replaces) = if let Some(res) = resume {
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

    let native_session = resume
        .map(str::to_owned)
        .or_else(|| (harness == "claude").then(new_uuid));
    let label = safe_label(options.get("task"));
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
    session_lock.check()?;
    let attempt = event_id(repo, &start_args, services, stop)?;
    let member = attempt.clone();

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
    if let Err(err) = services
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
        for (opt, val) in [
            ("@oms_panel_main_attempt", attempt.as_str()),
            ("@oms_panel_room", room_id.as_str()),
            ("@oms_panel_native_pane", pane.as_str()),
            ("@oms_panel_room_participant", member.as_str()),
            ("@oms_panel_repo", canon.as_str()),
        ] {
            let _ = terminals::tmux(
                services,
                repo,
                &crate::commands::strings(&["set-option", "-w", "-t", &pane, opt, val]),
                stop,
            );
        }
    }

    let argv = native_command(
        policy,
        harness,
        repo,
        resume,
        options.get("model"),
        options.get("task"),
        Some(&room_id),
        Some(&member),
        native_session.as_deref(),
    )?;
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Error::new("empty native command"))?;
    let mut cmd = Command::new(program);
    cmd.args(rest)
        .current_dir(&execution)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("OMS_PANEL_OWNER", &resolution.owner)
        .env("OMS_PANEL_HARNESS", harness)
        .env("OMS_ROOM_ID", &room_id)
        .env("OMS_ROOM_PARTICIPANT", &member)
        .env("OMS_ROOM_REPO", repo)
        .env("OMS_PANEL_MAIN_ATTEMPT", &attempt)
        .env("OMS_PANEL_RESULTS", "1");
    apply_main_model_env(&mut cmd, harness, resolution.model.as_deref());
    if ["codex", "claude", "antigravity"].contains(&resolution.owner.as_str()) {
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

    let admission = (|| -> Result<oms_command_peer::admission::InteractiveCall> {
        let call = oms_command_peer::admission::begin_interactive_with_executor(
            oms_command_peer::admission::InteractiveRequest {
                state_root: repo,
                provider: &resolution.owner,
                selected_model: resolution.model.as_deref(),
                account: None,
                attempt: Some(&attempt),
                resume_session: resume,
            },
            oms_command_peer::admission::InteractiveContext {
                contract_repo: &execution,
                execution_repo: &execution,
                root: Some(oms_command_peer::process::RootLocator {
                    owner: &attempt,
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
        Ok(call)
    })();
    let call = match admission {
        Ok(call) => call,
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
    let mut child = match cmd.spawn() {
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
            return Err(Error::new(format!("native launch failed: {err}")));
        }
    };

    let status_code = wait_with_heartbeat(&mut child, repo, &attempt, services, stop);
    let accounting = call.finish(None, None).map_err(admission_error);
    let final_states: &[&str] = match status_code {
        0 => &["verifying", "review", "done"],
        129 | 130 | 143 => &["cancelled"],
        _ => &["failed"],
    };
    let settling = AtomicBool::new(false);
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

    let mut mode = PaneMode::Control;
    let mut pane_repo: Option<String> = None;
    let value_flags = [
        "--repo", "--room", "--host", "--layout", "--view", "--owner", "--launch", "--resume",
        "--model", "--task", "--count",
    ];
    while idx < words.len() {
        let word = words[idx].as_str();
        let (key, inline) = word
            .split_once('=')
            .map_or((word, None), |(key, value)| (key, Some(value)));
        idx += 1;
        if key == "--watch" && inline.is_none() {
            mode = PaneMode::Watch;
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
            } else if key == "--launch" && mode == PaneMode::Control {
                mode = PaneMode::Native;
            }
        } else {
            return None;
        }
    }
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
    let mut argv = crate::commands::strings(&[
        "split-window",
        "-f",
        "-h",
        "-b",
        "-l",
        "28%",
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
    policy: &routing::Policy,
    session: &str,
    harness: &str,
    view: &str,
    attention_only: bool,
    resume: Option<&str>,
    model: Option<&str>,
    task: Option<&str>,
    room_id: &str,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    let execution = state::native_repository(repo, None)?;
    let cmd = launch_command(
        repo,
        prefix,
        session,
        harness,
        resume,
        model,
        task,
        Some(room_id),
    )?;
    let mut argv = crate::commands::strings(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("={session}:"),
        "-n",
        harness,
        "-c",
    ]);
    argv.push(execution.into());
    argv.push(cmd.into());
    let pane = terminals::tmux(services, repo, &argv, stop)?;
    bind_window(
        &pane,
        policy,
        harness,
        view,
        attention_only,
        model,
        room_id,
        services,
        repo,
        stop,
    )?;
    let owner = resolve_main(policy, harness, model).owner;
    let watch = watch_shell(repo, prefix, Some(&owner), None, Some(room_id));
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

#[allow(clippy::too_many_arguments)]
fn bind_window(
    pane: &str,
    policy: &routing::Policy,
    harness: &str,
    view: &str,
    attention_only: bool,
    model: Option<&str>,
    room_id: &str,
    services: &dyn Services,
    repo: &Path,
    stop: &AtomicBool,
) -> Result<()> {
    let owner = resolve_main(policy, harness, model).owner;
    let board_view = if view == "auto" { "graph" } else { view };
    let att = if attention_only { "1" } else { "0" };
    for (k, v) in [
        ("@oms_panel_owner", owner.as_str()),
        ("@oms_panel_harness", harness),
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

#[allow(clippy::too_many_arguments)]
fn launch_command(
    repo: &Path,
    prefix: &[String],
    session: &str,
    launch: &str,
    resume: Option<&str>,
    model: Option<&str>,
    task: Option<&str>,
    room_id: Option<&str>,
) -> Result<String> {
    let state = repo
        .to_str()
        .ok_or_else(|| Error::new("tmux launch requires a UTF-8 state path"))?;
    let mut parts = vec!["env".into()];
    for name in ["OMS_STATE_REPO", "OMS_ROOM_REPO", "OMS_ROOM_ID"]
        .iter()
        .chain(NATIVE_CONTEXT_ENV)
    {
        parts.extend(["-u".into(), (*name).into()]);
    }
    parts.extend(panel_env_prefix_for(launch == "agy").into_iter().skip(1));
    parts.push(format!("OMS_STATE_REPO={state}"));
    parts.push(format!("OMS_ROOM_REPO={state}"));
    for name in NATIVE_CONTEXT_ENV {
        if let Some(value) = std::env::var_os(name) {
            let value = value.into_string().map_err(|_| {
                Error::new(format!(
                    "{name} is not UTF-8; native launch context cannot be relayed"
                ))
            })?;
            parts.push(format!("{name}={value}"));
        }
    }
    parts.push(format!("OMS_PANEL_SESSION={session}"));
    if let Some(id) = room_id {
        parts.push(format!("OMS_ROOM_ID={id}"));
    }
    parts.extend_from_slice(prefix);
    parts.extend([
        "panel".into(),
        "--repo".into(),
        repo.to_string_lossy().into_owned(),
        "--host".into(),
        "inline".into(),
        "--launch".into(),
        launch.into(),
    ]);
    for (flag, val) in [("--resume", resume), ("--model", model), ("--task", task)] {
        if let Some(v) = val {
            if v.starts_with("--") {
                parts.push(format!("{flag}={v}"));
            } else {
                parts.extend([flag.into(), v.into()]);
            }
        }
    }
    Ok(format!(
        "exec {}",
        parts
            .iter()
            .map(|p| crate::commands::shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn watch_shell(
    repo: &Path,
    prefix: &[String],
    owner: Option<&str>,
    session: Option<&str>,
    room: Option<&str>,
) -> String {
    let mut parts = panel_env_prefix_for(owner.is_none_or(|o| o == "antigravity" || o == "agy"));
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
fn staged_dispatch_args(source: &Path, exe: &Path) -> Option<(String, String)> {
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
    if harness == "agy" {
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

fn panel_env_prefix_for(keep_agy_model: bool) -> Vec<String> {
    let mut out = vec!["env".into()];
    for name in PANEL_ENV_PASSTHROUGH {
        if !keep_agy_model && (*name == "OMS_MODEL" || *name == "OMS_ANTIGRAVITY_SETTINGS") {
            continue;
        }
        if let Ok(val) = std::env::var(name) {
            out.push(format!("{name}={val}"));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn native_command(
    policy: &routing::Policy,
    provider: &str,
    repo: &Path,
    resume: Option<&str>,
    model: Option<&str>,
    task: Option<&str>,
    room_id: Option<&str>,
    member: Option<&str>,
    session_id: Option<&str>,
) -> Result<Vec<String>> {
    let provider = normalize_harness(provider)?;
    let permissions = match std::env::var("OMS_PANEL_MAIN_PERMISSIONS") {
        Ok(val) if val == "full" || val == "native" => val,
        Ok(_) => {
            return Err(Error::new(
                "OMS_PANEL_MAIN_PERMISSIONS must be full or native",
            ));
        }
        Err(_) => "full".into(),
    };
    let resolution = resolve_main(policy, provider, model);
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
    match provider {
        "agy" => {
            require(
                resume.is_none(),
                "Agy resume needs a verified native session binding; start a fresh Agy main",
            )?;
            let prompt = if let Some(t) = task {
                format!("{base_inst}\nUser task (data): {}", Value::String(t.into()))
            } else {
                base_inst
            };
            let mut argv = vec!["agy".into(), "--prompt-interactive".into(), prompt];
            if permissions == "full" {
                argv.push("--dangerously-skip-permissions".into());
            }
            if let Some(m) = resolution.model
                && model.is_some()
            {
                argv.extend(["--model".into(), m]);
            }
            Ok(argv)
        }
        "codex" => {
            let prompt = if let Some(t) = task {
                format!("{base_inst}\nUser task (data): {}", Value::String(t.into()))
            } else {
                base_inst
            };
            let mut argv = vec!["codex".into()];
            if let Some(r) = resume {
                argv.extend(["resume".into(), r.into()]);
            }
            if permissions == "full" {
                argv.push("--dangerously-bypass-approvals-and-sandbox".into());
            }
            if let Some(m) = resolution.model {
                argv.extend(["--model".into(), m]);
            }
            if model.is_none() {
                argv.extend([
                    "--config".into(),
                    format!("model_reasoning_effort=\"{}\"", resolution.effort),
                ]);
            }
            argv.push(prompt);
            Ok(argv)
        }
        "claude" => {
            let mut argv = vec!["claude".into()];
            if permissions == "full" {
                argv.push("--dangerously-skip-permissions".into());
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
            if let Some(m) = resolution.model {
                argv.extend(["--model".into(), m]);
            }
            if model.is_none() {
                argv.extend(["--effort".into(), resolution.effort]);
            }
            argv.extend(["--append-system-prompt".into(), base_inst]);
            if let Some(t) = task {
                argv.extend(["--".into(), t.into()]);
            }
            Ok(argv)
        }
        other => Err(Error::new(format!("unsupported launch provider: {other}"))),
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

fn resolve_main(policy: &routing::Policy, harness: &str, model: Option<&str>) -> MainResolution {
    if harness == "agy" {
        return MainResolution {
            owner: "antigravity".into(),
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
    let preset = policy.main.get(harness);
    MainResolution {
        owner: harness.into(),
        model: model
            .map(str::to_owned)
            .or_else(|| preset.map(|p| p.model.clone())),
        effort: preset.map_or_else(|| "medium".into(), |p| p.effort.clone()),
    }
}

fn ensure_main_installed(harness: &str) -> Result<()> {
    require(
        which_exists(harness),
        &format!(
            "{} main needs the {harness} CLI on PATH",
            capitalize(harness)
        ),
    )
}

pub(crate) fn normalize_harness(raw: &str) -> Result<&'static str> {
    match raw {
        "claude" => Ok("claude"),
        "codex" => Ok("codex"),
        "agy" | "antigravity" => Ok("agy"),
        "pi" => Err(Error::new(
            "Pi is not an Overall Main option; choose codex, claude or agy. Pi remains available through oms harness pi and worker routes",
        )),
        _ => Err(Error::new(format!(
            "unsupported Main harness: {raw}; choose codex, claude or agy"
        ))),
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
    let bound = tmux_quiet(
        services,
        repo,
        &["show-option", "-w", "-v", "-t", &base.pane, "@oms_panel_room"],
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
        &["show", "--id", requested, "--json"],
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

fn release_dead_mains(repo: &Path, services: &dyn Services, stop: &AtomicBool) -> Result<()> {
    let raw = crate::commands::query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let classified = release::classify(repo, rows(&raw), services);
    for item in rows(&classified["releasable"]) {
        if let Some(id) = item["attempt"].as_str() {
            let _ = release::release(repo, id, services, stop);
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
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<()> {
    release_dead_mains(repo, services, stop)?;
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
            || RECORDED_HARNESSES.contains(name)
            || *name == "antigravity";
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
mod native_lifecycle_contract {
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

    #[test]
    fn native_main_picker_is_explicit_and_handles_unavailable_and_cancelled_choices() {
        use super::MainSelection;
        use crate::input::Key;

        assert_eq!(super::main_selection(None).unwrap(), MainSelection::Prompt);
        for raw in ["", " \t "] {
            assert_eq!(
                super::main_selection(Some(raw)).unwrap(),
                MainSelection::Board
            );
        }
        for (raw, harness) in [
            (" claude ", "claude"),
            ("codex", "codex"),
            ("agy", "agy"),
            ("antigravity", "agy"),
        ] {
            assert_eq!(
                super::main_selection(Some(raw)).unwrap(),
                MainSelection::Launch(harness)
            );
        }
        for raw in ["pi", "grok", "auto"] {
            assert!(super::main_selection(Some(raw)).is_err());
        }

        for (key, expected) in [
            (Key::Char('1'), Some("claude")),
            (Key::Char('2'), Some("codex")),
            (Key::Char('3'), Some("agy")),
            (Key::Enter, None),
            (Key::Eof, None),
            (Key::Escape, None),
        ] {
            let mut output = Vec::new();
            let result = super::pick_main(
                &mut |_| Ok(vec![key.clone()]),
                &mut output,
                &[true; 3],
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(result, expected);
            assert!(!String::from_utf8(output).unwrap().contains("Pi"));
        }
        let mut keys = vec![Key::Char('1'), Key::Char('3')].into_iter();
        let mut output = Vec::new();
        assert_eq!(
            super::pick_main(
                &mut |_| Ok(vec![keys.next().expect("picker ignored an available choice")]),
                &mut output,
                &[false, false, true],
                &AtomicBool::new(false),
            )
            .unwrap(),
            Some("agy")
        );
        assert!(String::from_utf8(output).unwrap().contains("Claude CLI is not on PATH"));
        let stop = AtomicBool::new(false);
        let error = super::pick_main(
            &mut |_| {
                stop.store(true, Ordering::Release);
                Ok(vec![Key::Char('2')])
            },
            &mut Vec::new(),
            &[true; 3],
            &stop,
        )
        .unwrap_err();
        assert_eq!(error.code, 130);
    }

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

    struct RealCliParsers;
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
                super::run_native(&repo, &policy, harness, &options, &svc, &stop).unwrap(),
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
        let result = super::run_native(&repo, &policy, harness, &options, &svc, &stop);
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
                super::run_native(&repo, &policy, harness, &options, &svc, &stop)
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
        assert!(super::run_native(&repo, &policy, harness, &options, &svc, &stop).is_err());
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
            let running =
                scope.spawn(|| super::run_native(&repo, &policy, harness, &options, &svc, &stop));
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
                .env("OMS_PANEL_MAIN", "pi")
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
        let other = TempRepo::new();
        let options = crate::cli::Options::parse(&crate::commands::strings(&[
            "--repo",
            repo.0.to_str().unwrap(),
            "--host",
            "inline",
            "--room",
            "room-fixture",
        ]))
        .unwrap();
        for cwd in [repo.0.parent().unwrap(), other.0.as_path()] {
            let (resolved, execution) =
                crate::commands::resolve_repositories(&options, cwd, None, true).unwrap();
            assert_eq!(resolved.as_deref(), Some(repo.0.as_path()));
            assert_eq!(execution.as_deref(), Some(repo.0.as_path()));
            let error =
                super::default_main(&repo.0, cwd, &repo.0, &options, &svc, &stop).unwrap_err();
            assert!(error.message.contains("Pi is not an Overall Main option"));
            assert!(!repo.0.join(".oms").exists());
            assert!(
                crate::commands::resolve_repositories(&options, cwd, Some(&repo.0), true).is_ok()
            );
            let error =
                crate::commands::resolve_repositories(&options, cwd, Some(&other.0), true)
                    .unwrap_err();
            assert!(error.message.contains("conflicts with OMS_ROOM_REPO"));
        }
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
            let names = ["OMS_STATE_REPO", "OMS_ROOM_REPO", "OMS_ROOM_ID"]
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

        let launched = super::launch_command(
            &dir.0,
            &prefix,
            "oms-test",
            "codex",
            None,
            None,
            None,
            Some("room-1234"),
        );
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
        let seen: serde_json::Value = serde_json::from_slice(&fs::read(snapshot).unwrap()).unwrap();
        assert_eq!(seen["OMS_STATE_REPO"], dir.0.to_str().unwrap());
        assert_eq!(seen["OMS_ROOM_REPO"], dir.0.to_str().unwrap());
        assert_eq!(seen["OMS_ROOM_ID"], "room-1234");
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
            let command = super::launch_command(
                &dir.0,
                &prefix,
                "oms-test",
                "codex",
                None,
                None,
                Some(task),
                Some("room-1234"),
            )
            .unwrap();
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
        );
        assert_eq!(super::match_panel_command(&watch_cmd, &dir.0), Some(true));
        let legacy_pi = format!(
            "oms panel --repo {} --launch pi",
            crate::commands::shell_quote(&dir.0.to_string_lossy())
        );
        assert_eq!(
            super::match_panel_entry(&legacy_pi, &dir.0),
            Some(super::PaneMode::Native)
        );

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
            closed: bool,
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
                } else if args.last().is_some_and(|a| a == "@oms_panel_room") {
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
                            "id":"room-1", "closed":self.closed,
                            "participants": if self.matrix {
                                serde_json::json!([{"participant":"main-1","role":"main","joined":true}])
                            } else {
                                serde_json::json!([])
                            }
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
            closed: false,
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
            closed: true,
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
            closed: false,
            ..closed
        };
        assert!(super::check_legacy_main(&dir.0, "oms-test", None, &pending, &stop).is_err());
        super::check_legacy_main(&dir.0, "oms-test", Some("@1"), &pending, &stop).unwrap();
        for terminal in [false, true] {
            for reference in [None, Some(Value::from("")), Some(Value::from("foreign"))] {
                let svc = PendingWindowMock {
                    reference: reference.clone(),
                    terminal,
                    matrix: true,
                    control_only: false,
                    closed: false,
                };
                let expected = usize::from(!terminal || reference == Some(Value::from("foreign")));
                assert_eq!(
                    super::live_mains(&dir.0, "oms-test", Some("room-1"), None, &svc, &stop)
                        .unwrap(),
                    expected,
                    "terminal={terminal} reference={reference:?}"
                );
                if !terminal {
                    let svc = PendingWindowMock {
                        reference: reference.clone(),
                        terminal,
                        matrix: true,
                        control_only: true,
                        closed: false,
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
}
