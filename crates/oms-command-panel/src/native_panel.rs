//! Native interactive main panel launch, reopen, split construction and window spawning.
use crate::{
    Error, Output, Result, Services, cli::Options, json, parent_only, require, rows, text,
};
use oms_panel_core::{
    NativeCommand, Store, Value, host, release, routing, services::ExecutionRequest, state,
    terminals,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_LIVE_MAINS: usize = 1;
const LAUNCH_HARNESSES: &[&str] = &["claude", "codex", "pi", "agy"];
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
    transport: String,
}

/// Produce `--dry-run` launch plan JSON without spawning processes or opening panels.
pub fn dry_run_plan(
    repo: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let harness = match options.get("launch") {
        Some(raw) => Some(normalize_harness(raw, repo, services, stop)?),
        None => None,
    };
    if harness.as_deref() == Some("pi") && options.has("resume") {
        return Err(Error::new(
            "Pi resume needs a verified Pi session binding; start a new Pi main",
        ));
    }
    if harness.as_deref() == Some("agy") && options.has("resume") {
        return Err(Error::new(
            "Agy resume needs a verified native session binding; start a fresh Agy main",
        ));
    }
    let split = wants_split(options);
    let policy = load_policy(source)?;
    let resolution = harness
        .as_deref()
        .map(|h| resolve_main(&policy, h, options.get("model")));
    let launchable = resolution
        .as_ref()
        .is_some_and(|r| !(harness.as_deref() == Some("pi") && r.transport == "direct"));
    let argv = if let (Some(h), true) = (harness.as_deref(), launchable) {
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
        .filter(|row| !row.watch && current.is_none_or(|c| row.window == c.window))
        .collect();
    require(
        candidates.len() == 1,
        "no unique existing OMS input pane; reopen from the intended OMS window",
    )?;
    let base = candidates[0].clone();
    let fresh = discover_panel_panes(repo, services, stop)?;
    require(
        fresh.contains(&base),
        "OMS input pane changed; retry after refreshing",
    )?;
    let boards: Vec<&PanelPane> = fresh
        .iter()
        .filter(|row| row.window == base.window && row.watch)
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
        let canon = std::fs::canonicalize(repo)
            .unwrap_or_else(|_| repo.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let owns_main = pane_option(services, repo, "@oms_panel_main_attempt", stop)
            .is_some_and(|v| !v.is_empty())
            && pane_option(services, repo, "@oms_panel_room_participant", stop)
                .is_some_and(|v| !v.is_empty())
            && pane_option(services, repo, "@oms_panel_repo", stop).as_deref()
                == Some(canon.as_str());
        let cmd = watch_shell(repo, &prefix, None, Some(&base.session));
        board_split(&base.pane, repo, &cmd, owns_main, services, stop)?;
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
    let harness = normalize_harness(raw_provider, repo, services, stop)?;
    let policy = load_policy(source)?;
    let model = options.get("model");
    let resolution = resolve_main(&policy, &harness, model);
    ensure_main_installed(&harness, model, &policy)?;
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
        check_legacy_main(repo, &session, services, stop)?;
        sess_lock.check()?;
    }

    start_lock.check()?;
    let pane = add_main_window(
        repo,
        &prefix,
        &policy,
        &session,
        &harness,
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
        let cap = capitalize(&harness);
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
    let harness = normalize_harness(raw_provider, repo, services, stop)?;
    if harness == "pi" && options.has("resume") {
        return Err(Error::new(
            "Pi resume needs a verified Pi session binding; start a new Pi main",
        ));
    }
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
    ensure_main_installed(&harness, options.get("model"), &policy)?;

    if wants_split(options) {
        require(
            which_exists("tmux"),
            "native chat board requires tmux on PATH",
        )?;
        split_panel(repo, &prefix, &policy, &harness, options, services, stop)?;
        return Ok((0, vec![], vec![]));
    }
    let code = run_native(repo, &policy, &harness, options, services, stop)?;
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

fn split_panel(
    repo: &Path,
    prefix: &[String],
    policy: &routing::Policy,
    harness: &str,
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
        if let Some(bound) = bound_rooms.iter().next() {
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
            check_legacy_main(repo, &session, services, stop)?;
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
            check_legacy_main(repo, &session, services, stop)?;
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
            let candidate = chosen_room.or_else(|| bound.into_iter().next());
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
        );
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
        let watch = watch_shell(repo, prefix, Some(&owner), None);
        board_split(&pane, repo, &watch, true, services, stop)?;
        terminals::tmux(
            services,
            repo,
            &crate::commands::strings(&["select-pane", "-t", &pane]),
            stop,
        )?;
    }

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
    let resolution = resolve_main(policy, harness, options.get("model"));
    require(
        !(harness == "pi" && resolution.transport == "direct"),
        "selected main transport launch is not available yet",
    )?;
    let store = Store::open(repo)?;
    let session_lock = store.transaction("panel-main-session", Duration::from_millis(100))?;
    let session = std::env::var("OMS_PANEL_SESSION").unwrap_or_default();
    if !session.is_empty() {
        check_legacy_main(repo, &session, services, stop)?;
        session_lock.check()?;
    }

    let execution = state::native_repository(repo, None)?;
    let resume = options.get("resume");
    let chosen_room = crate::commands::room(options);
    let room_id = if let Some(res) = resume {
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
                .or_else(|| bound_rooms.iter().next().map(String::as_str)),
            services,
            stop,
        )?;
        if let Some(bound) = bound_rooms.iter().next() {
            require(
                bound == &ident,
                "native session is bound to another room; resume refused",
            )?;
        }
        ident
    } else {
        session_lock.check()?;
        ensure_room(repo, chosen_room.as_deref(), services, stop)?
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
    if harness != "pi" && ["codex", "claude", "antigravity"].contains(&resolution.owner.as_str()) {
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

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            let _ = transition_attempt(
                repo,
                &attempt,
                &["failed"],
                Some("native_launch_failed"),
                services,
                stop,
            );
            return Err(Error::new(format!("native launch failed: {err}")));
        }
    };

    let status_code = wait_with_heartbeat(&mut child, repo, &attempt, services, stop);
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
        stop,
    );
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
    watch: bool,
    signature: String,
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
        if let Some(watch) = match_panel_command(command, &canon_repo) {
            found.push(PanelPane {
                session: session.into(),
                window: window.into(),
                pane: pane.into(),
                watch,
                signature: oms_panel_core::digest(command.as_bytes()),
            });
        }
    }
    Ok(found)
}

fn match_panel_command(command: &str, canon_repo: &Path) -> Option<bool> {
    let mut words = split_shell_words(command)?;
    if words.len() == 1 {
        words = split_shell_words(&words[0])?;
    }
    if words.first().is_some_and(|w| w == "exec") {
        words.remove(0);
    }
    if words.first().is_some_and(|w| w == "env") {
        words.remove(0);
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
        if words.get(1).map(String::as_str) != Some("panel") {
            return None;
        }
        2
    } else {
        return None;
    };

    let mut watch = false;
    let mut pane_repo: Option<String> = None;
    let value_flags = [
        "--repo", "--room", "--host", "--layout", "--view", "--owner", "--launch", "--resume",
        "--model", "--task", "--count",
    ];
    while idx < words.len() {
        let key = words[idx].as_str();
        idx += 1;
        if key == "--watch" {
            watch = true;
        } else if matches!(key, "--no-animation" | "--attention-only") {
            continue;
        } else if value_flags.contains(&key) && idx < words.len() {
            let val = &words[idx];
            idx += 1;
            if key == "--repo" {
                pane_repo = Some(val.clone());
            }
        } else {
            return None;
        }
    }
    let resolved = std::fs::canonicalize(pane_repo?).ok()?;
    if resolved == canon_repo {
        Some(watch)
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
    );
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
    let watch = watch_shell(repo, prefix, Some(&owner), None);
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
) -> String {
    let mut parts = panel_env_prefix_for(launch == "agy");
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
            parts.extend([flag.into(), v.into()]);
        }
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

fn watch_shell(
    repo: &Path,
    prefix: &[String],
    owner: Option<&str>,
    session: Option<&str>,
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
    format!(
        "exec {}",
        parts
            .iter()
            .map(|p| crate::commands::shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn entry_prefix(source: &Path) -> Result<Vec<String>> {
    let entry = crate::executable::resolve(source)
        .ok_or_else(|| Error::new("oms panel executable unavailable"))?;
    let mut prefix = Vec::new();
    if entry.script {
        prefix.push("bash".into());
    }
    prefix.push(entry.path.to_string_lossy().into_owned());
    Ok(prefix)
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
            "\nYou are joined to OMS room {id} as {m}. Read oms room show --id {id} first. Include --room {id} in every oms panel --dispatch or --council command."
        ),
        _ => String::new(),
    };
    let base_inst = format!(
        "OMS terminal panel sole main. Repository: {}. Use --owner {} for owner commands.{room_inst}",
        repo.display(),
        resolution.owner
    );
    match provider {
        "pi" => {
            require(
                resume.is_none(),
                "Pi resume needs a verified Pi session binding; start a new Pi main",
            )?;
            require(
                resolution.transport == "gateway",
                "direct Pi main launch is not available yet",
            )?;
            let mut argv = vec![
                "oms".into(),
                "harness".into(),
                "pi".into(),
                "--model".into(),
                resolution.model.unwrap_or_default(),
                "--effort".into(),
                resolution.effort,
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
            Ok(argv)
        }
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
    if harness == "pi" {
        let preset = policy.main.get("codex");
        return MainResolution {
            owner: "codex".into(),
            model: model
                .map(str::to_owned)
                .or_else(|| preset.map(|p| p.model.clone())),
            effort: preset.map_or_else(|| "medium".into(), |p| p.effort.clone()),
            transport: "gateway".into(),
        };
    }
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
            transport: "native".into(),
        };
    }
    let preset = policy.main.get(harness);
    MainResolution {
        owner: harness.into(),
        model: model
            .map(str::to_owned)
            .or_else(|| preset.map(|p| p.model.clone())),
        effort: preset.map_or_else(|| "medium".into(), |p| p.effort.clone()),
        transport: "native".into(),
    }
}

fn ensure_main_installed(
    harness: &str,
    model: Option<&str>,
    policy: &routing::Policy,
) -> Result<()> {
    let ok = which_exists(harness)
        && (harness != "pi"
            || resolve_main(policy, harness, model).transport != "gateway"
            || which_exists("codex"));
    if ok {
        return Ok(());
    }
    if harness == "pi" {
        Err(Error::new(
            "Pi main needs the Pi CLI and Codex CLI on PATH (Pi uses Codex inference)",
        ))
    } else {
        Err(Error::new(format!(
            "{} main needs the {harness} CLI on PATH",
            capitalize(harness)
        )))
    }
}

fn normalize_harness(
    raw: &str,
    repo: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    if LAUNCH_HARNESSES.contains(&raw) {
        return Ok(raw.into());
    }
    let report = crate::commands::query(
        services,
        NativeCommand::ProviderCatalog,
        repo,
        &["--providers", raw, "--json"],
        stop,
    )?;
    let list = report.get("providers").unwrap_or(&report);
    let provider = rows(list)
        .first()
        .map(|row| text(row, "provider"))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new(format!("unsupported launch provider: {raw}")))?;
    Ok(provider.to_owned())
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

fn check_legacy_main(
    repo: &Path,
    session: &str,
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
            live_mains(repo, session, None, None, services, stop)? < MAX_LIVE_MAINS,
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
    let raw = terminals::tmux(
        services,
        repo,
        &crate::commands::strings(&[
            "list-windows",
            "-t",
            &format!("={session}"),
            "-F",
            "#{@oms_panel_owner}\t#{@oms_panel_room}\t#{@oms_panel_room_participant}\t#{window_name}\t#{window_id}\t#{@oms_panel_harness}",
        ]),
        stop,
    )?;
    let mut windows = Vec::new();
    for line in raw.replace('\r', "").lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 6
            || fields[3].trim().is_empty()
            || !crate::commands::valid_target(fields[4], '@')
        {
            return Err(Error::new(
                "main window status unavailable; existing sessions preserved",
            ));
        }
        let native = !fields[2].is_empty()
            || !fields[5].is_empty()
            || (!fields[0].is_empty() && fields[3] != "control")
            || LAUNCH_HARNESSES.contains(&fields[3])
            || fields[3] == "antigravity";
        if !native {
            if fields[3] != "control" {
                return Err(Error::new(
                    "main window ownership is unverified; existing sessions preserved",
                ));
            }
            continue;
        }
        if pending_window.is_some_and(|pw| pw == fields[4] && fields[2].is_empty()) {
            continue;
        }
        windows.push((fields[1].to_owned(), fields[2].to_owned()));
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
            let who = row["refs"]["panel_room_participant"]
                .as_str()
                .or_else(|| row["attempt_id"].as_str())
                .unwrap_or("")
                .to_owned();
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
) -> Result<BTreeSet<String>> {
    let digest = &oms_panel_core::digest(resume.as_bytes())[..32];
    let raw = crate::commands::query(
        services,
        NativeCommand::Room,
        repo,
        &["discover", "--json"],
        stop,
    )?;
    require(
        raw["incomplete"] != true,
        "native resume ownership discovery is incomplete; inspect existing sessions",
    )?;
    let mut bound = BTreeSet::new();
    for state in rows(&raw["rooms"]) {
        for enrolled in rows(&state["participants"]) {
            if enrolled["provider"] == provider
                && enrolled["joined"] == true
                && enrolled["consumer"] == digest
                && let Some(id) = state["id"].as_str()
            {
                bound.insert(id.to_owned());
            }
        }
    }
    Ok(bound)
}

fn pane_option(
    services: &dyn Services,
    repo: &Path,
    option: &str,
    stop: &AtomicBool,
) -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    if pane.is_empty() {
        return None;
    }
    tmux_quiet(
        services,
        repo,
        &["show-option", "-w", "-v", "-t", &pane, option],
        stop,
    )
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
                NativeCommand::Room => oms_command_room::run(args, repo, stop),
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
}
